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

"""Reference groundedness verifier — a small, fully-offline sidecar the platform
is the HTTP client of (like the reranker / OCR engines). Small models, all
licence-clean for commercial use, all on CPU:

  * **LettuceDetect** (MIT, ModernBERT) — self-highlights unsupported token spans
    over (context, question, answer). The primary LIVE detector (Mode A). No LLM.
  * **NLI cross-encoder** (cross-encoder/nli-deberta-v3-small, Apache-2.0) — labels
    each flagged span **contradicted** (the source disagrees) vs **not_mentioned**
    (the source is silent). A cross-encoder, NOT an LLM-judge (spec §4.3).
  * **FactCG** (yaxili96/FactCG-DeBERTa-v3-Large, MIT) — the per-claim DRAFT verifier
    (Mode B): (evidence, claim) → supported vs not. Binary, so for a not-supported
    claim the NLI cross-encoder above splits contradicted vs not_mentioned. FactCG
    is **lazy-loaded** on the first /v1/verify-claims call so Mode-A-only deployments
    stay light.
  * **HHEM-2.1-open** (vectara/hallucination_evaluation_model, Apache-2.0) — a cheap
    factual-consistency scorer (premise, hypothesis) → [0,1]. Two uses: an optional
    second-opinion that rescues a flagged item HHEM judges supported (`hhem_filter`),
    and the `/v1/hhem` endpoint the benchmark harness scores against. Lazy-loaded.

The sidecar produces clean, whole-word, labelled spans + per-claim verdicts;
`ml/app/verify.py` derives the scores, so swapping a model never changes a formula.

Run (CPU; ~400 MB ModernBERT + ~280 MB DeBERTa NLI + ~600 MB DeBERTa-v3-large
FactCG + ~1.2 GB HHEM, downloaded once on first use):

    pip install -r requirements.txt
    VERIFY_PORT=8095 python server.py

Zero-egress: after the one-time model download the service makes no outbound
calls. Pin the models + versions in the deployment profile (like the LLM)."""

import os
import re

import torch
import uvicorn
from fastapi import FastAPI
from pydantic import BaseModel
from transformers import AutoModelForSequenceClassification, AutoTokenizer

from lettucedetect.models.inference import HallucinationDetector

MODEL = os.environ.get("VERIFY_MODEL", "KRLabsOrg/lettucedect-large-modernbert-en-v1")
NLI_MODEL = os.environ.get("VERIFY_NLI_MODEL", "cross-encoder/nli-deberta-v3-small")
FACTCG_MODEL = os.environ.get("VERIFY_FACTCG_MODEL", "yaxili96/FactCG-DeBERTa-v3-Large")
HHEM_MODEL = os.environ.get("VERIFY_HHEM_MODEL", "vectara/hallucination_evaluation_model")
PORT = int(os.environ.get("VERIFY_PORT", "8095"))

# HHEM consistency at/above which a flagged item is rescued as supported (§5).
HHEM_KEEP = 0.5

# FactCG's authoritative inference template (github.com/derenlei/FactCG,
# factcg/utils.py). The (evidence, claim) pair is wrapped in this single string
# — NOT a bare two-segment encode; without it the model does not discriminate.
_FACTCG_TEMPLATE = (
    '{text_a}\n\nChoose your answer: based on the paragraph above can we conclude '
    'that "{text_b}"?\n\nOPTIONS:\n- Yes\n- No\nI think the answer is '
)

app = FastAPI(title="PAI groundedness verifier", version="1.2")

# Loaded once at boot. transformer method = token-level span highlighting.
_detector = HallucinationDetector(method="transformer", model_path=MODEL)

# Small NLI cross-encoder for the contradicted-vs-not-mentioned label. Labels come
# from the model's own config (typically contradiction / entailment / neutral).
_nli_tok = AutoTokenizer.from_pretrained(NLI_MODEL)
_nli = AutoModelForSequenceClassification.from_pretrained(NLI_MODEL)
_nli.eval()
_NLI_LABELS = {int(k): str(v).lower() for k, v in _nli.config.id2label.items()}

# FactCG (Mode B claim verifier) — lazy-loaded on first use.
_factcg_tok = None
_factcg = None
_factcg_sup_idx = 1  # index of the "supported" class; resolved at load


def _ensure_factcg() -> None:
    """Load FactCG on first claim request. Resolve the 'supported' class from the
    model's id2label (entail/support/consistent/factual); else the binary
    convention (higher index = supported)."""
    global _factcg_tok, _factcg, _factcg_sup_idx
    if _factcg is not None:
        return
    _factcg_tok = AutoTokenizer.from_pretrained(FACTCG_MODEL)
    _factcg = AutoModelForSequenceClassification.from_pretrained(FACTCG_MODEL)
    _factcg.eval()
    labels = {int(k): str(v).lower() for k, v in _factcg.config.id2label.items()}
    sup = next(
        (i for i, n in labels.items()
         if any(t in n for t in ("support", "entail", "consistent", "factual", "true"))),
        None,
    )
    _factcg_sup_idx = sup if sup is not None else max(labels)


@torch.no_grad()
def _factcg_supported(evidence: str, claim: str) -> tuple[bool, float]:
    """FactCG verdict for one claim against its evidence → (supported?, prob_supported).
    Uses FactCG's own instruction template + single-string encode (truncate the
    evidence, not the claim); index 1 = consistent/supported."""
    _ensure_factcg()
    text = _FACTCG_TEMPLATE.format(text_a=evidence, text_b=claim)
    enc = _factcg_tok(text, max_length=2048, truncation=True, return_tensors="pt")
    logits = _factcg(**enc).logits
    prob_sup = float(torch.softmax(logits, dim=-1)[0][_factcg_sup_idx].item())
    return int(logits.argmax(-1).item()) == _factcg_sup_idx, prob_sup


# HHEM-2.1-open (factual-consistency scorer) — lazy-loaded on first use.
_hhem = None


def _ensure_hhem() -> None:
    global _hhem
    if _hhem is not None:
        return
    _hhem = AutoModelForSequenceClassification.from_pretrained(HHEM_MODEL, trust_remote_code=True)
    _hhem.eval()


@torch.no_grad()
def _hhem_scores(pairs: list[tuple[str, str]]) -> list[float]:
    """Factual-consistency [0,1] per (premise, hypothesis) pair — 1 = fully supported."""
    if not pairs:
        return []
    _ensure_hhem()
    return [float(x) for x in _hhem.predict(pairs)]


class VerifyRequest(BaseModel):
    context: list[str]  # the retrieved evidence the answer must rest on
    question: str
    answer: str
    hhem_filter: bool = False  # HHEM second opinion: rescue spans HHEM judges supported


def _clean_spans(answer: str, raw: list[dict]) -> list[dict]:
    """Snap each token-level span out to whole-word boundaries, trim whitespace,
    then merge overlapping / whitespace-adjacent spans — so a word like
    'Compliance' is never sliced into 'Com' / 'ance'."""
    n = len(answer)
    snapped: list[tuple[int, int, float]] = []
    for p in raw:
        try:
            start = max(0, min(int(p["start"]), n))
            end = max(0, min(int(p["end"]), n))
        except (KeyError, TypeError, ValueError):
            continue
        if end <= start:
            continue
        conf = float(p.get("confidence", p.get("score", 0.0)))
        while start > 0 and answer[start - 1].isalnum():
            start -= 1
        while end < n and answer[end].isalnum():
            end += 1
        while start < end and answer[start].isspace():
            start += 1
        while end > start and answer[end - 1].isspace():
            end -= 1
        if end > start:
            snapped.append((start, end, conf))
    if not snapped:
        return []
    snapped.sort(key=lambda t: t[0])
    merged: list[list] = [list(snapped[0])]
    for start, end, conf in snapped[1:]:
        last = merged[-1]
        if start <= last[1] or answer[last[1]:start].strip() == "":
            last[1] = max(last[1], end)
            last[2] = max(last[2], conf)
        else:
            merged.append([start, end, conf])
    return [{"start": s, "end": e, "text": answer[s:e], "confidence": c} for s, e, c in merged]


_SENT = re.compile(r"[^.!?]+[.!?]*")


@torch.no_grad()
def _label(premise: str, hypothesis: str) -> str:
    """NLI verdict for one flagged span: 'contradicted' if the source disagrees,
    else 'not_mentioned' (we never relabel a flagged span as supported). A merged
    span can mix a contradiction with merely-unsupported claims, so we classify
    each sentence and let the more severe verdict win — any sentence the source
    contradicts makes the whole span 'contradicted'."""
    parts = [p.strip() for p in _SENT.findall(hypothesis) if p.strip()] or [hypothesis]
    for part in parts:
        enc = _nli_tok(premise, part, truncation=True, max_length=512, return_tensors="pt")
        pred = _NLI_LABELS.get(int(_nli(**enc).logits.argmax(-1).item()), "")
        if pred.startswith("contradict"):
            return "contradicted"
    return "not_mentioned"


class Claim(BaseModel):
    text: str
    evidence: str = ""


class VerifyClaimsRequest(BaseModel):
    claims: list[Claim]
    hhem_filter: bool = False  # HHEM second opinion: rescue claims HHEM judges supported


class HhemRequest(BaseModel):
    pairs: list[list[str]]  # [[premise, hypothesis], …]


@app.get("/version")
def version() -> dict:
    return {
        "model": MODEL,
        "nli_model": NLI_MODEL,
        "factcg_model": FACTCG_MODEL,
        "hhem_model": HHEM_MODEL,
        "method": "transformer",
        "version": "1.3",
    }


@app.post("/v1/hhem")
def hhem(req: HhemRequest) -> dict:
    """Factual-consistency [0,1] per (premise, hypothesis) pair (HHEM-2.1-open).
    Used by the benchmark harness to score (source, summary) pairs."""
    pairs = [(p[0], p[1]) for p in req.pairs if len(p) >= 2]
    return {"scores": _hhem_scores(pairs)}


@app.post("/v1/verify-claims")
def verify_claims(req: VerifyClaimsRequest) -> dict:
    """Per-claim verdict for Mode B (Verify draft). FactCG decides supported vs
    not; for a not-supported claim the NLI cross-encoder splits contradicted vs
    not_mentioned. A claim with no bound evidence cannot be supported."""
    verdicts = []
    for c in req.claims:
        evidence = (c.evidence or "").strip()
        if not evidence:
            verdicts.append({"verdict": "not_mentioned", "score": 0.0})
            continue
        supported, prob_sup = _factcg_supported(evidence, c.text)
        if supported:
            verdicts.append({"verdict": "supported", "score": prob_sup})
        else:
            verdicts.append({"verdict": _label(evidence, c.text), "score": 1.0 - prob_sup})
    # HHEM second opinion: a flagged claim HHEM judges consistent is rescued.
    if req.hhem_filter:
        idx = [i for i, v in enumerate(verdicts)
               if v["verdict"] != "supported" and (req.claims[i].evidence or "").strip()]
        if idx:
            scores = _hhem_scores([(req.claims[i].evidence, req.claims[i].text) for i in idx])
            for i, sc in zip(idx, scores):
                if sc >= HHEM_KEEP:
                    verdicts[i] = {"verdict": "supported", "score": sc}
    return {"verdicts": verdicts}


@app.post("/v1/verify")
def verify(req: VerifyRequest) -> dict:
    """Return clean, whole-word spans of `answer` unsupported by `context`, each
    labelled contradicted vs not_mentioned. Empty list = fully grounded."""
    raw = _detector.predict(
        context=req.context,
        question=req.question,
        answer=req.answer,
        output_format="spans",
    )
    premise = " ".join(req.context)
    spans = [
        {**s, "label": _label(premise, s["text"])}
        for s in _clean_spans(req.answer, raw)
    ]
    # HHEM second opinion: drop a flagged span HHEM judges consistent (most reliable
    # for whole claims; Mode A spans are fragments, so use with care).
    if req.hhem_filter and spans:
        scores = _hhem_scores([(premise, s["text"]) for s in spans])
        spans = [s for s, sc in zip(spans, scores) if sc < HHEM_KEEP]
    return {"spans": spans}


if __name__ == "__main__":
    uvicorn.run(app, host="0.0.0.0", port=PORT)
