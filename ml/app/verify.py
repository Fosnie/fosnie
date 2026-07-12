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

"""Groundedness verifier behind a swappable interface.
Mode A (live): send the
retrieved context + question + streamed answer to a small cross-encoder
(LettuceDetect) that self-highlights unsupported token spans — no claim
decomposition, no LLM in the hot path. Returns the flagged spans plus a derived
groundedness score.

Like the reranker, it degrades gracefully and self-heals: a failure starts a
short cooldown, after which it retries. The verifier being down NEVER affects an
answer — the score path just fails open (no spans, score=None). The sidecar
returns raw spans only; the score formula lives HERE so swapping the model never
changes the score. Dev engine: backend/deploy/verify (port 8095)."""

import logging
import re
import time

from . import http_client
from .config import settings

_log = logging.getLogger("pai.verify")
_RETRY_AFTER = 30.0  # seconds to skip the verifier after a failure, then retry
_down_until = 0.0
_warned = False

_EMPTY = {"spans": [], "score": None, "total": 0, "flagged": 0}


def _sentence_count(text: str) -> int:
    """Rough claim/sentence count for the denominator — terminal punctuation or
    newlines. At least 1 for any non-empty answer."""
    parts = [p for p in re.split(r"[.!?]\s+|\n+", text.strip()) if p.strip()]
    return max(len(parts), 1)


async def verify_live(
    context: str,
    question: str,
    answer: str,
    model: str | None = None,
    strictness: str = "strict",
    threshold: float = 0.0,
    hhem_filter: bool = False,
) -> dict:
    """Verify a streamed RAG answer against its retrieved context. Returns
    `{spans, score, total, flagged, model}` where each span carries char offsets
    into `answer` + a `label` (contradicted | not_mentioned), `score` ∈ [0,1] is
    the grounded fraction (1 − flagged_chars / answer_chars), `total` ≈ sentence
    count, `flagged` = span count. The sidecar returns clean, whole-word, labelled
    spans. On failure or when disabled, fails open (empty, score=None)."""
    global _down_until, _warned
    from .rag_ctx import cfg

    chosen = model or cfg("verify_model", settings.verify_model)
    if not cfg("verify_enabled", settings.verify_enabled):
        return {**_EMPTY, "model": chosen}
    answer = answer or ""
    if not answer.strip() or not context.strip():
        return {**_EMPTY, "model": chosen}
    if time.monotonic() < _down_until:
        return {**_EMPTY, "model": chosen}  # cooling down after a recent failure

    url = f"{cfg('verify_base_url', settings.verify_base_url).rstrip('/')}/v1/verify"
    payload = {
        "context": [context], "question": question, "answer": answer,
        "model": chosen, "hhem_filter": hhem_filter,
    }
    headers = {"Authorization": f"Bearer {cfg('verify_api_key', settings.verify_api_key)}"}
    try:
        client = http_client.get_client()
        r = await client.post(url, json=payload, headers=headers, timeout=settings.verify_timeout)
        r.raise_for_status()
        data = r.json().get("spans", [])
        # The sidecar already cleaned (whole-word) and labelled each span. Clamp
        # defensively to the answer bounds and normalise the label.
        spans = []
        flagged_chars = 0
        for s in data:
            try:
                start = max(0, int(s["start"]))
                end = min(len(answer), int(s["end"]))
            except (KeyError, TypeError, ValueError):
                continue
            if end <= start:
                continue
            label = s.get("label", "not_mentioned")
            if label not in ("contradicted", "not_mentioned"):
                label = "not_mentioned"
            conf = float(s.get("confidence", s.get("score", 0.0)))
            # Confidence floor: a low-confidence flag is treated as grounded.
            if conf < threshold:
                continue
            # Lenient strictness: only a contradiction fails — a not-mentioned span
            # is tolerated (neither flagged nor counted against the score).
            if strictness == "lenient" and label != "contradicted":
                continue
            spans.append({
                "start": start,
                "end": end,
                "text": s.get("text", answer[start:end]),
                "label": label,
                "score": conf,
            })
            flagged_chars += end - start
        score = max(0.0, min(1.0, 1.0 - flagged_chars / max(len(answer), 1)))
        if _warned:
            _log.info("verifier recovered")
            _warned = False
        return {
            "spans": spans,
            "score": score,
            "total": _sentence_count(answer),
            "flagged": len(spans),
            "model": chosen,
        }
    except Exception as e:  # noqa: BLE001 — degrade, never break the answer
        _down_until = time.monotonic() + _RETRY_AFTER
        if not _warned:
            _log.warning(
                "verifier unavailable (%ss cooldown), groundedness skipped: %s",
                int(_RETRY_AFTER),
                e,
            )
            _warned = True
        return {**_EMPTY, "model": chosen}


async def verify_claims(pairs: list[dict], hhem_filter: bool = False) -> list[dict]:
    """Mode B per-claim verification: each `pair` is {text, evidence}. Returns one
    `{verdict, score}` per pair, verdict ∈ supported | contradicted | not_mentioned
    (FactCG decides supported/not; NLI splits the rest). Chunked to keep each
    sidecar call bounded; fails open (every claim → not_mentioned/0) so a verifier
    outage never aborts the draft job — it just yields an empty verdict."""
    from .rag_ctx import cfg

    if not pairs:
        return []
    fallback = [{"verdict": "not_mentioned", "score": 0.0} for _ in pairs]
    if not cfg("verify_enabled", settings.verify_enabled):
        return fallback

    url = f"{cfg('verify_base_url', settings.verify_base_url).rstrip('/')}/v1/verify-claims"
    headers = {"Authorization": f"Bearer {cfg('verify_api_key', settings.verify_api_key)}"}
    batch = max(1, settings.verify_claims_batch)
    out: list[dict] = []
    try:
        client = http_client.get_client()
        for i in range(0, len(pairs), batch):
            chunk = pairs[i : i + batch]
            payload = {
                "claims": [
                    {"text": p.get("text", ""), "evidence": p.get("evidence", "")} for p in chunk
                ],
                "hhem_filter": hhem_filter,
            }
            r = await client.post(url, json=payload, headers=headers, timeout=settings.verify_timeout)
            r.raise_for_status()
            verdicts = r.json().get("verdicts", [])
            # Guard against a short response — pad with not_mentioned.
            for j in range(len(chunk)):
                v = verdicts[j] if j < len(verdicts) else {}
                verd = v.get("verdict", "not_mentioned")
                if verd not in ("supported", "contradicted", "not_mentioned"):
                    verd = "not_mentioned"
                out.append({"verdict": verd, "score": float(v.get("score", 0.0))})
        return out
    except Exception as e:  # noqa: BLE001 — degrade, never abort the job
        _log.warning("verify-claims unavailable, claims unscored: %s", e)
        return fallback
