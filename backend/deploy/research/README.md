# Deep Research benchmark (RACE + FACT)

Quality harness for Deep Research reports.
Drives the **live** pipeline end-to-end and scores each report on:

- **FACT** — citation accuracy (the run's own in-pipeline verification:
  `supported / total`, the FactCG check the report shipped with).
- **RACE** — an LLM judge answers **binary** rubric items (DRB-II style) across
  comprehensiveness / insight / instruction-following / readability, then a
  **length penalty** (`× min(1, target/actual_words)`) so padding can't buy
  score. The "free of redundancy" item is forced false when the structural pass
  flags padding (not LLM-trusted).
- **STRUCTURE** — deterministic: ≥3 sections, every `[W#]`/`[D#]` resolves to a
  References entry.

Gated (slow on a local LLM). It measures the **deployed LLM + pipeline on this
fixture set** — honest only when scoped + dated; never "no hallucinations".

```sh
PAI_RESEARCH_EVAL=1 \
LLM_BASE_URL=http://localhost:11434/v1 LLM_MODEL=qwen3 \
ML_BASE_URL=http://localhost:8090 \
python backend/deploy/research/benchmark.py --date 2026-06-11 --limit 2
```

Needs: the ML service (`:8090`) up; the verifier sidecar (`:8095`) up and
`research.verify` enabled (else FACT is null); SearXNG for web fixtures. Swap
`fixtures.jsonl` for your own representative question set (add `source: "files"`
+ `kb_ids` for corpus fixtures). Outputs dated `report.{json,md}`.
