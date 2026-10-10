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
import math
import re
import time

from . import http_client
from .config import settings
from .inference_fixed import routed_fixed, post
from .inference_target import strict

_log = logging.getLogger("fosnie.verify")
_RETRY_AFTER = 30.0  # seconds to skip the verifier after a failure, then retry
_down_until = 0.0
_warned = False

_EMPTY = {"spans": [], "score": None, "total": 0, "flagged": 0}


def _routed_spans(data, answer, strictness, threshold):
    """Malformed evidence cannot be silently converted into a grounded answer."""
    if not isinstance(data, dict) or not isinstance(data.get("spans"), list) or len(data["spans"]) > 1024:
        raise ValueError("invalid verifier spans")
    spans = []
    for item in data["spans"]:
        if not isinstance(item, dict):
            raise ValueError("invalid verifier span")
        start, end = item.get("start"), item.get("end")
        confidence = item.get("confidence", item.get("score"))
        if (type(start) is not int or type(end) is not int or not 0 <= start < end <= len(answer)
                or item.get("label") not in ("contradicted", "not_mentioned")
                or type(confidence) not in (int, float) or not math.isfinite(confidence) or not 0 <= confidence <= 1
                or "text" in item and item["text"] != answer[start:end]):
            raise ValueError("invalid verifier span")
        spans.append(dict(start=start, end=end, text=answer[start:end], label=item["label"], score=confidence))
    spans.sort(key=lambda s: (s["start"], s["end"]))
    if any(left["end"] > right["start"] for left, right in zip(spans, spans[1:])):
        raise ValueError("overlapping verifier spans")
    return [s for s in spans if s["score"] >= threshold and (strictness == "strict" or s["label"] == "contradicted")]


def _sentence_count(text: str) -> int:
    """Rough claim/sentence count for the denominator — terminal punctuation or
    newlines. At least 1 for any non-empty answer."""
    parts = [p for p in re.split(r"[.!?]\s+|\n+", text.strip()) if p.strip()]
    return max(len(parts), 1)


@routed_fixed("verify")
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

    if strict():
        if (strictness not in ("strict", "lenient") or not math.isfinite(threshold) or not 0 <= threshold <= 1
                or hhem_filter):
            raise ValueError("invalid or unadopted routed verification options")
        for text, limit in ((context, 1024 * 1024), (question, 65536), (answer, 262144)):
            if not text.strip() or len(text.encode("utf-8")) > limit:
                raise ValueError("empty or oversized routed verification input")
    chosen = model or cfg("verify_model", settings.verify_model)
    if strict() and chosen != cfg("verify_model", None):
        raise ValueError("verification model conflicts with routing target")
    if not cfg("verify_enabled", settings.verify_enabled):
        return {**_EMPTY, "model": chosen}
    answer = answer or ""
    if not answer.strip() or not context.strip():
        return {**_EMPTY, "model": chosen}
    if not strict() and time.monotonic() < _down_until:
        return {**_EMPTY, "model": chosen}  # cooling down after a recent failure

    url = f"{cfg('verify_base_url', settings.verify_base_url).rstrip('/')}/v1/verify"
    payload = {
        "context": [context], "question": question, "answer": answer,
        "model": chosen, "hhem_filter": hhem_filter,
    }
    headers = http_client.bearer_headers(cfg('verify_api_key', settings.verify_api_key))
    try:
        client = http_client.get_client()
        r = await post("verify", client, url, json=payload, headers=headers, timeout=settings.verify_timeout)
        r.raise_for_status()
        if strict():
            spans = _routed_spans(r.json(), answer, strictness, threshold)
            return {"spans": spans, "score": 1.0 - sum(s["end"] - s["start"] for s in spans) / len(answer),
                    "total": _sentence_count(answer), "flagged": len(spans), "model": chosen}
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
        if strict():
            raise
        _down_until = time.monotonic() + _RETRY_AFTER
        if not _warned:
            _log.warning(
                "verifier unavailable (%ss cooldown), groundedness skipped: %s",
                int(_RETRY_AFTER),
                e,
            )
            _warned = True
        return {**_EMPTY, "model": chosen}


@routed_fixed("verify_claims")
async def verify_claims(pairs: list[dict], hhem_filter: bool = False) -> list[dict]:
    """Mode B per-claim verification: each `pair` is {text, evidence}. Returns one
    `{verdict, score}` per pair, verdict ∈ supported | contradicted | not_mentioned
    (FactCG decides supported/not; NLI splits the rest). Chunked to keep each
    sidecar call bounded; fails open (every claim → not_mentioned/0) so a verifier
    outage never aborts the draft job — it just yields an empty verdict."""
    from .rag_ctx import cfg

    if not pairs:
        return []
    if strict():
        import math
        if hhem_filter or len(pairs) > 16 or any(
            not isinstance(p.get("text"), str) or not p["text"].strip() or len(p["text"].encode("utf-8")) > 4096
            or not isinstance(p.get("evidence"), str) or len(p["evidence"].encode("utf-8")) > 16384 for p in pairs):
            raise ValueError("invalid bounded claim verification")
    fallback = [{"verdict": "not_mentioned", "score": 0.0} for _ in pairs]
    if not cfg("verify_enabled", settings.verify_enabled):
        return fallback

    url = f"{cfg('verify_base_url', settings.verify_base_url).rstrip('/')}/v1/verify-claims"
    headers = http_client.bearer_headers(cfg('verify_api_key', settings.verify_api_key))
    batch = cfg("verify_claims_batch", settings.verify_claims_batch)
    if strict() and (type(batch) is not int or not 1 <= batch <= 16):
        raise ValueError("invalid bounded verifier batch size")
    batch = max(1, batch)
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
            if strict():
                payload["model"] = cfg("verify_model", None)
            r = await post("verify_claims", client, url, json=payload, headers=headers, timeout=settings.verify_timeout)
            r.raise_for_status()
            verdicts = r.json().get("verdicts", [])
            if strict():
                if not isinstance(verdicts, list) or len(verdicts) != len(chunk):
                    raise ValueError("claim verifier returned the wrong number of verdicts")
                for v in verdicts:
                    if (not isinstance(v, dict) or v.get("verdict") not in ("supported", "contradicted", "not_mentioned")
                            or type(v.get("score")) not in (int, float) or not math.isfinite(v["score"])
                            or not 0 <= v["score"] <= 1):
                        raise ValueError("invalid claim verdict")
            # Guard against a short response — pad with not_mentioned.
            for j in range(len(chunk)):
                v = verdicts[j] if j < len(verdicts) else {}
                verd = v.get("verdict", "not_mentioned")
                if verd not in ("supported", "contradicted", "not_mentioned"):
                    verd = "not_mentioned"
                out.append({"verdict": verd, "score": float(v.get("score", 0.0))})
        return out
    except Exception as e:  # noqa: BLE001 — degrade, never abort the job
        if strict():
            raise
        _log.warning("verify-claims unavailable, claims unscored: %s", e)
        return fallback
