"""Web-search quality eval.

Gated on PAI_WEB_EVAL=1 — needs the real stack (SearXNG reachable, the network,
the local LLM for planning/grading and for the judge, the reranker). Runs the
standard-depth agentic loop over ~20 representative queries across four classes
(simple facts, multi-hop, freshness, conflict-prone) and scores each digest with
a single-call LLM judge (factual accuracy, citation support, completeness, source
quality). Report-only — prints a per-query + per-group table; the single hard
assert is a liveness floor (≥80% of queries return a non-empty digest with ≥1
citation), so the judge's opinions never make CI flaky.

Run: `PAI_WEB_EVAL=1 .venv/Scripts/python -m pytest tests/eval/test_web_eval.py -q -s`
"""

import asyncio
import json
import os
import pathlib
import sys
from datetime import date

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))

pytestmark = pytest.mark.skipif(
    os.getenv("PAI_WEB_EVAL") != "1",
    reason="web eval needs the live stack + network (set PAI_WEB_EVAL=1)",
)

# (class, query, recency). Freshness queries lean on today's date in the judge.
_QUERIES = [
    ("fact", "What is the capital city of Australia?", "any"),
    ("fact", "Who wrote the novel Nineteen Eighty-Four?", "any"),
    ("fact", "What is the chemical symbol for tungsten?", "any"),
    ("fact", "How many time zones does Russia span?", "any"),
    ("fact", "What is the speed of light in a vacuum in metres per second?", "any"),
    ("multihop", "Which company makes the GPUs used in the Nintendo Switch, and where is that company headquartered?", "any"),
    ("multihop", "Who is the current CEO of the company that owns Instagram, and what year did they found it?", "any"),
    ("multihop", "What is the tallest mountain in the country that hosted the 2016 Summer Olympics?", "any"),
    ("multihop", "Which programming language was created by the same person who created Clojure's host platform?", "any"),
    ("multihop", "What is the population of the capital of the country where the Danube delta meets the sea?", "any"),
    ("fresh", "What is the latest stable version of the Rust programming language?", "month"),
    ("fresh", "What is the most recent release of Python 3?", "month"),
    ("fresh", "What was the most recent major version of PostgreSQL released?", "year"),
    ("fresh", "What is the latest Long Term Support release of Ubuntu?", "year"),
    ("fresh", "What is the current version of the TypeScript compiler?", "month"),
    ("conflict", "Is coffee good or bad for your health?", "any"),
    ("conflict", "Does a low-carbohydrate diet outperform a low-fat diet for weight loss?", "any"),
    ("conflict", "Are nuclear power stations a safe source of energy?", "any"),
    ("conflict", "Is intermittent fasting effective for long-term weight loss?", "any"),
    ("conflict", "Do violent video games cause real-world aggression?", "any"),
]

_JUDGE_SYSTEM = (
    "You are a strict evaluator of a web-search answer. Given a question and a digest "
    "of web sources with numbered citations, rate the digest 1-5 on each axis. "
    "Return ONLY JSON: {\"factual_accuracy\": n, \"citation_support\": n, "
    "\"completeness\": n, \"source_quality\": n}. 5 = excellent, 1 = poor."
)


async def _judge(question: str, digest: str) -> dict:
    from app import llm

    today = date.today().isoformat()
    try:
        out = await llm.complete(
            _JUDGE_SYSTEM,
            f"Today is {today}.\nQuestion: {question}\n\nDigest:\n{digest}",
            max_tokens=120,
        )
        start, end = out.find("{"), out.rfind("}")
        obj = json.loads(out[start : end + 1]) if start >= 0 else {}
        return {k: float(obj.get(k, 0)) for k in
                ("factual_accuracy", "citation_support", "completeness", "source_quality")}
    except Exception as e:  # noqa: BLE001 — a judge miss must not crash the eval
        print(f"[eval] judge failed: {e}")
        return {k: 0.0 for k in
                ("factual_accuracy", "citation_support", "completeness", "source_quality")}


async def _run_all() -> list[dict]:
    from app.web import pipeline

    rows: list[dict] = []
    for cls, q, recency in _QUERIES:
        result = await pipeline.web_search(q, recency=recency, depth="standard")
        digest = result.get("digest", "")
        n_cit = len(result.get("citations", []))
        scores = await _judge(q, digest) if n_cit else {
            k: 0.0 for k in ("factual_accuracy", "citation_support", "completeness", "source_quality")
        }
        rows.append({"class": cls, "query": q, "citations": n_cit, "scores": scores})
    return rows


def test_web_search_eval():
    rows = asyncio.run(_run_all())

    print("\n=== Web-search eval ===")
    print(f"{'class':<9} {'cit':>3}  {'fact':>4} {'cite':>4} {'comp':>4} {'src':>4}  query")
    by_class: dict[str, list[float]] = {}
    for r in rows:
        s = r["scores"]
        mean = sum(s.values()) / 4
        by_class.setdefault(r["class"], []).append(mean)
        print(
            f"{r['class']:<9} {r['citations']:>3}  "
            f"{s['factual_accuracy']:>4.1f} {s['citation_support']:>4.1f} "
            f"{s['completeness']:>4.1f} {s['source_quality']:>4.1f}  {r['query'][:60]}"
        )
    print("\n--- group means (overall 1-5) ---")
    for cls, means in by_class.items():
        print(f"{cls:<9} {sum(means) / len(means):.2f}  (n={len(means)})")

    # Liveness floor only — the judge's scores are reported, not gated.
    answered = sum(1 for r in rows if r["citations"] >= 1 and r["scores"])
    ratio = answered / len(rows)
    print(f"\nanswered with ≥1 citation: {answered}/{len(rows)} ({ratio:.0%})")
    assert ratio >= 0.8, f"only {ratio:.0%} of eval queries returned a cited digest"
