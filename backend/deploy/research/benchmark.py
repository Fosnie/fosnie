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

"""Deep Research quality benchmark: the
full RACE + FACT panel with DRB-II binary rubrics and an anti-verbosity (length-
normalised) aggregate.

Per fixture question it runs the REAL pipeline end-to-end (POST the deployed ML
`/deep_research` with `verify=true`), then scores the report on three axes:

  • FACT  — citation accuracy: the run's own in-pipeline verification
    (supported / total), the same FactCG check the report shipped with.
  • RACE  — an LLM judge answers BINARY items (DRB-II style — binary defuses the
    judge's verbosity bias) across four dimensions (comprehensiveness, insight,
    instruction-following, readability); each dimension = mean(items), RACE =
    mean(dimensions), then a LENGTH PENALTY `× min(1, target_words/actual_words)`
    so padding cannot buy score. The 'free of redundancy' readability item is
    forced false (not LLM-trusted) when the structural pass flags padding.
  • STRUCTURE — deterministic checks (headings present, every [W#]/[D#] resolves
    to a References entry, per-section citation-density floor).

This measures the DEPLOYED LLM + pipeline on THIS fixture set — honest only when
scoped + dated; never a claim of "no hallucinations". Swap in your own fixtures
for a representative number.

Stdlib only (urllib). Gated — set PAI_RESEARCH_EVAL=1 to run (it drives the live
stack and is slow on a local LLM):

    PAI_RESEARCH_EVAL=1 LLM_BASE_URL=http://localhost:11434/v1 LLM_MODEL=qwen3 \
    ML_BASE_URL=http://localhost:8090 \
    python backend/deploy/research/benchmark.py --date 2026-06-11 --limit 2
"""

import argparse
import json
import os
import re
import statistics
import sys
import urllib.request

LLM_BASE = os.environ.get("LLM_BASE_URL", "http://localhost:11434/v1")
LLM_MODEL = os.environ.get("LLM_MODEL", "qwen3")
LLM_KEY = os.environ.get("LLM_API_KEY", "ollama")
ML_BASE = os.environ.get("ML_BASE_URL", "http://localhost:8090")

_HERE = os.path.dirname(os.path.abspath(__file__))
_WID = re.compile(r"\[([WD]\d+)\]")
_H2 = re.compile(r"^## \d+\. .+$", re.MULTILINE)

# DRB-II binary rubric: each item is answered true/false; binary scoring + the
# length penalty below is the anti-verbosity control (judges reward padding).
_RUBRIC = {
    "comprehensiveness": [
        "The report addresses the research question directly.",
        "It covers the main sub-topics a domain reader would expect.",
        "Claims are specific (figures, names, dates) rather than vague generalities.",
    ],
    "insight": [
        "It compares or contrasts options/sources rather than only listing them.",
        "It identifies tensions, trade-offs, or open questions.",
    ],
    "instruction_following": [
        "The structure matches the requested template.",
        "The voice/register fits the template (e.g. formal vs exploratory).",
    ],
    "readability": [
        "Sections are well-organised and flow logically.",
        "The prose is free of repetition and padding.",  # forced false on padding flags
    ],
}


def _post(url: str, payload: dict, headers: dict | None = None, timeout: int = 1800) -> bytes:
    data = json.dumps(payload).encode()
    req = urllib.request.Request(
        url, data=data, method="POST",
        headers={"content-type": "application/json", **(headers or {})},
    )
    with urllib.request.urlopen(req, timeout=timeout) as r:  # noqa: S310 — internal hosts
        return r.read()


def run_report(fix: dict) -> dict:
    """Drive the live pipeline; return the terminal `done` event."""
    body = {
        "question": fix["question"],
        "template": fix.get("template", "exploration"),
        "depth": fix.get("depth", "standard"),
        "source": fix.get("source", "web"),
        "kb_ids": fix.get("kb_ids", []),
        "verify": True,
    }
    raw = _post(f"{ML_BASE.rstrip('/')}/deep_research", body)
    done = {}
    for line in raw.splitlines():
        line = line.strip()
        if not line:
            continue
        ev = json.loads(line)
        if ev.get("type") == "done":
            done = ev
        elif ev.get("type") == "error":
            return {"error": ev.get("message", "unknown")}
    return done


def judge_race(question: str, template: str, report_md: str) -> dict:
    """One judge call → strict-JSON binary answers per rubric item."""
    items = [{"dim": d, "i": i, "item": it} for d, its in _RUBRIC.items() for i, it in enumerate(its)]
    spec = "\n".join(f'{x["dim"]}.{x["i"]}: {x["item"]}' for x in items)
    sys_prompt = (
        "You are a strict research-report judge. Answer each item EXACTLY true or false "
        "based ONLY on the report. Do NOT reward length — a longer report is not better. "
        'Return ONLY JSON mapping each "dim.i" key to a boolean, e.g. {"insight.0": true}.'
    )
    user = f"Question: {question}\nTemplate: {template}\n\nItems:\n{spec}\n\nReport:\n{report_md[:12000]}"
    raw = _post(
        f"{LLM_BASE.rstrip('/')}/chat/completions",
        {"model": LLM_MODEL, "messages": [
            {"role": "system", "content": sys_prompt}, {"role": "user", "content": user}],
         "temperature": 0, "max_tokens": 512, "stream": False},
        {"Authorization": f"Bearer {LLM_KEY}"},
    )
    text = json.loads(raw)["choices"][0]["message"]["content"] or ""
    s, e = text.find("{"), text.rfind("}")
    answers = json.loads(text[s : e + 1]) if s >= 0 else {}
    return {f'{x["dim"]}.{x["i"]}': bool(answers.get(f'{x["dim"]}.{x["i"]}', False)) for x in items}


def structure_check(report_md: str) -> dict:
    """Deterministic: section count, citation resolution, density floor."""
    body, _, refs = report_md.partition("## References")
    sections = _H2.findall(body)
    used = set(_WID.findall(body))
    ref_ids = set(_WID.findall(refs))
    unresolved = sorted(used - ref_ids)  # a marker with no reference entry
    return {
        "sections": len(sections),
        "citations_used": len(used),
        "unresolved": unresolved,
        "ok": len(sections) >= 3 and not unresolved,
    }


def score_fixture(fix: dict, limit_words_floor: int = 400) -> dict:
    done = run_report(fix)
    if done.get("error") or not done.get("report_md"):
        return {"question": fix["question"], "error": done.get("error", "no report")}
    report = done["report_md"]
    template = fix.get("template", "exploration")
    actual_words = len(re.sub(r"## References.*", "", report, flags=re.S).split())

    struct = structure_check(report)
    verification = done.get("verification") or {}
    total = verification.get("total", 0)
    fact = (verification.get("supported", 0) / total) if total else None

    answers = judge_race(fix["question"], template, report)
    # Padding signal forces the redundancy item false (anti-verbosity).
    padded = "did not meet structural targets" in report
    if padded:
        answers["readability.1"] = False
    dims = {}
    for d, its in _RUBRIC.items():
        vals = [1.0 if answers.get(f"{d}.{i}") else 0.0 for i in range(len(its))]
        dims[d] = statistics.mean(vals) if vals else 0.0
    race = statistics.mean(dims.values()) if dims else 0.0
    # Length normalisation: a report far over its band is penalised.
    target = max(limit_words_floor, struct["sections"] * 600)
    length_factor = min(1.0, target / max(actual_words, 1))
    adjusted = race * length_factor

    return {
        "question": fix["question"], "template": template, "source": fix.get("source", "web"),
        "words": actual_words, "structure": struct,
        "fact_citation_accuracy": round(fact, 4) if fact is not None else None,
        "race_dims": {k: round(v, 3) for k, v in dims.items()},
        "race": round(race, 4), "length_factor": round(length_factor, 3),
        "race_adjusted": round(adjusted, 4), "padding_flagged": padded,
    }


def main() -> None:
    if os.environ.get("PAI_RESEARCH_EVAL") != "1":
        print("PAI_RESEARCH_EVAL != 1 — skipping the live Deep Research benchmark.")
        sys.exit(0)
    ap = argparse.ArgumentParser(description="Deep Research RACE+FACT benchmark.")
    ap.add_argument("--data", default=os.path.join(_HERE, "fixtures.jsonl"))
    ap.add_argument("--date", default="unknown")
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--out", default=_HERE)
    args = ap.parse_args()

    fixtures = []
    with open(args.data, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                fixtures.append(json.loads(line))
    if args.limit:
        fixtures = fixtures[: args.limit]

    rows = []
    for i, fix in enumerate(fixtures):
        print(f"[{i + 1}/{len(fixtures)}] {fix['question'][:60]}…")
        try:
            rows.append(score_fixture(fix))
        except Exception as e:  # noqa: BLE001 — a bad fixture must not sink the run
            rows.append({"question": fix.get("question", "?"), "error": str(e)})
        r = rows[-1]
        if "error" in r:
            print(f"    error: {r['error'][:80]}")
        else:
            print(f"    RACE={r['race']:.2f} adj={r['race_adjusted']:.2f} "
                  f"FACT={r['fact_citation_accuracy']} struct_ok={r['structure']['ok']}")

    scored = [r for r in rows if "error" not in r]
    agg = {
        "n": len(scored),
        "race_mean": round(statistics.mean(r["race"] for r in scored), 4) if scored else 0.0,
        "race_adjusted_mean": round(statistics.mean(r["race_adjusted"] for r in scored), 4) if scored else 0.0,
        "fact_mean": round(statistics.mean(r["fact_citation_accuracy"] for r in scored
                                           if r["fact_citation_accuracy"] is not None), 4)
        if any(r["fact_citation_accuracy"] is not None for r in scored) else None,
        "structure_ok_rate": round(statistics.mean(1.0 if r["structure"]["ok"] else 0.0 for r in scored), 3) if scored else 0.0,
    }
    report = {"date": args.date, "llm_model": LLM_MODEL, "methodology":
              "RACE binary-rubric LLM judge (length-normalised) + FACT (FactCG supported/total) + deterministic structure",
              "aggregate": agg, "items": rows}
    with open(os.path.join(args.out, "report.json"), "w", encoding="utf-8") as f:
        json.dump(report, f, indent=2)
    with open(os.path.join(args.out, "report.md"), "w", encoding="utf-8") as f:
        f.write(f"# Deep Research benchmark — {args.date}\n\n")
        f.write(f"- **LLM**: {LLM_MODEL}  ·  **fixtures**: {len(rows)} ({agg['n']} scored)\n")
        f.write(f"- **RACE** (mean): {agg['race_mean']:.2f}  ·  **RACE length-adjusted**: {agg['race_adjusted_mean']:.2f}\n")
        f.write(f"- **FACT** citation accuracy (mean): {agg['fact_mean']}  ·  **structure-OK rate**: {agg['structure_ok_rate']:.2f}\n\n")
        f.write("| # | template | words | RACE | adj | FACT | struct |\n|---|---|---|---|---|---|---|\n")
        for i, r in enumerate(rows):
            if "error" in r:
                f.write(f"| {i + 1} | — | — | — | — | — | ERROR |\n")
            else:
                f.write(f"| {i + 1} | {r['template']} | {r['words']} | {r['race']:.2f} | "
                        f"{r['race_adjusted']:.2f} | {r['fact_citation_accuracy']} | "
                        f"{'ok' if r['structure']['ok'] else 'fail'} |\n")
        f.write("\n*Measures the deployed LLM + pipeline on THIS fixture set — scoped + dated. "
                "RACE uses binary rubrics + length normalisation to defuse verbosity bias; FACT is "
                "the report's own FactCG supported/total. Not a claim of 'no hallucinations'.*\n")
    print(f"\nRACE {agg['race_mean']:.2f} (adj {agg['race_adjusted_mean']:.2f}), "
          f"FACT {agg['fact_mean']} -> {os.path.join(args.out, 'report.md')}")


if __name__ == "__main__":
    main()
