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

"""Factual-consistency benchmark for the DEPLOYED LLM, by the Vectara
hallucination-leaderboard methodology:
for each source passage, ask the LLM to summarise it, then score
(source, summary) for factual consistency with HHEM-2.1-open via the verifier
sidecar's /v1/hhem. The mean is a measured, dated factual-consistency % for the
firm's deployment.

This measures the *served LLM*, NOT the span/claim detector — a different layer.
The number is honest only when scoped + dated; never market it as "no
hallucinations". Swap in your own evaluation set for a representative number.

Stdlib only (urllib) — run it from anywhere with Python 3.10+:

    LLM_BASE_URL=http://localhost:11434/v1 LLM_MODEL=llama3.2 \
    VERIFY_BASE_URL=http://localhost:8095 \
    python backend/deploy/verify/benchmark.py --date 2026-06-07
"""

import argparse
import json
import os
import statistics
import urllib.request

LLM_BASE = os.environ.get("LLM_BASE_URL", "http://localhost:11434/v1")
LLM_MODEL = os.environ.get("LLM_MODEL", "llama3.2")
LLM_KEY = os.environ.get("LLM_API_KEY", "ollama")
VERIFY_BASE = os.environ.get("VERIFY_BASE_URL", "http://localhost:8095")

_HERE = os.path.dirname(os.path.abspath(__file__))


def _post(url: str, payload: dict, headers: dict | None = None) -> dict:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data, method="POST",
        headers={"content-type": "application/json", **(headers or {})},
    )
    with urllib.request.urlopen(req, timeout=300) as r:  # noqa: S310 — internal hosts
        return json.loads(r.read())


def summarise(passage: str) -> str:
    out = _post(
        f"{LLM_BASE.rstrip('/')}/chat/completions",
        {
            "model": LLM_MODEL,
            "messages": [
                {"role": "system", "content": "Summarise the user's passage in 2-3 sentences. "
                 "Use ONLY information stated in the passage; add nothing and infer nothing."},
                {"role": "user", "content": passage},
            ],
            "temperature": 0, "max_tokens": 256, "stream": False,
        },
        {"Authorization": f"Bearer {LLM_KEY}"},
    )
    return (out["choices"][0]["message"]["content"] or "").strip()


def hhem_scores(pairs: list[list[str]]) -> list[float]:
    return _post(f"{VERIFY_BASE.rstrip('/')}/v1/hhem", {"pairs": pairs})["scores"]


def main() -> None:
    ap = argparse.ArgumentParser(description="LLM factual-consistency benchmark (HHEM).")
    ap.add_argument("--data", default=os.path.join(_HERE, "benchmark_data.jsonl"))
    ap.add_argument("--date", default="unknown", help="run date to stamp the report (YYYY-MM-DD)")
    ap.add_argument("--limit", type=int, default=0, help="cap the number of passages")
    ap.add_argument("--out", default=_HERE, help="output directory for report.{json,md}")
    args = ap.parse_args()

    passages: list[str] = []
    with open(args.data, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                passages.append(json.loads(line)["text"])
    if args.limit:
        passages = passages[: args.limit]

    rows = []
    for i, p in enumerate(passages):
        summary = summarise(p)
        score = hhem_scores([[p, summary]])[0]
        rows.append({"i": i, "summary": summary, "consistency": round(score, 4)})
        print(f"[{i + 1}/{len(passages)}] consistency={score:.3f}")

    mean = statistics.mean(r["consistency"] for r in rows) if rows else 0.0
    report = {
        "date": args.date, "llm_model": LLM_MODEL, "n": len(rows),
        "methodology": "Vectara hallucination-leaderboard (summarise → HHEM-2.1-open)",
        "factual_consistency": round(mean, 4),
        "hallucination_rate": round(1 - mean, 4),
        "items": rows,
    }
    with open(os.path.join(args.out, "report.json"), "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2)
    with open(os.path.join(args.out, "report.md"), "w", encoding="utf-8") as f:
        f.write("# Factual-consistency benchmark\n\n")
        f.write(f"- **Date**: {args.date}\n- **LLM**: {LLM_MODEL}\n- **Passages**: {len(rows)}\n")
        f.write("- **Methodology**: Vectara hallucination-leaderboard "
                "(summarise each passage → HHEM-2.1-open scores the (source, summary) pair)\n\n")
        f.write(f"## Factual consistency: {mean * 100:.1f}%  "
                f"(hallucination rate {(1 - mean) * 100:.1f}%)\n\n")
        f.write("| # | consistency | summary |\n|---|---|---|\n")
        for r in rows:
            f.write(f"| {r['i'] + 1} | {r['consistency']:.3f} | "
                    f"{r['summary'][:90].replace(chr(10), ' ')}… |\n")
        f.write("\n*Measures the served LLM's factual consistency on this set — a different layer "
                "from the span/claim detector. Scoped + dated; not a claim of 'no hallucinations'.*\n")
    print(f"\nFactual consistency: {mean * 100:.1f}%  -> {os.path.join(args.out, 'report.md')}")


if __name__ == "__main__":
    main()
