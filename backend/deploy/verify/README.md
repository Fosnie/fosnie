# Groundedness verifier — deployment

The "anti-hallucination" capability, scoped honestly as **verified groundedness**:
every factual claim in an output is
checked against **the sources the system actually holds** (the retrieved context),
and unsupported spans are flagged. Groundedness, **not truth** — it reduces, never
eliminates (spec §1, §11).

This directory is the **Mode A (live)** reference verifier: a small, fully-offline
sidecar the platform is the HTTP client of, exactly like the reranker / OCR engines.
It wraps **LettuceDetect** (`KRLabsOrg/lettucedect-large-modernbert-en-v1`, MIT,
ModernBERT base / Apache-2.0) which **self-highlights unsupported token spans** over
`(context, question, answer)` — no claim decomposition, no LLM in the hot path.

Enable with `features.groundedness = true` (backend boot config) **and** `VERIFY_*`
on the ML service (see `ml/.env.*.example`). If the verifier is absent the
groundedness path degrades silently (fail-open): the answer is unaffected, no pill
is shown — like the reranker falling back to hybrid order.

## Contract the platform targets

The verifier returns **raw spans only**; the ML service (`ml/app/verify.py`) derives
the groundedness score, so swapping the model never changes the score formula.

- **Verify (live spans, Mode A)** — `POST {VERIFY_BASE_URL}/v1/verify`
  `{ "context": ["…retrieved evidence…"], "question": "…", "answer": "…" }`
  → `{ "spans": [{ "start": int, "end": int, "text": str, "confidence": float, "label": str }] }`
  (`start`/`end` are character offsets into `answer`; `label` ∈ contradicted | not_mentioned;
  empty list = fully grounded).
- **Verify claims (per-claim, Mode B "Verify draft")** — `POST {VERIFY_BASE_URL}/v1/verify-claims`
  `{ "claims": [{ "text": "…claim…", "evidence": "…bound evidence…" }] }`
  → `{ "verdicts": [{ "verdict": "supported"|"contradicted"|"not_mentioned", "score": float }] }`.
  **FactCG** (`yaxili96/FactCG-DeBERTa-v3-Large`, MIT, ~600 MB, **lazy-loaded** on first call)
  decides supported vs not; the NLI cross-encoder splits a not-supported claim into
  contradicted/not_mentioned. ~0.5–1 s/claim on CPU — fine for the background draft job.
  Pass `"hhem_filter": true` (on either verify route) to add an **HHEM second opinion**: a
  flagged span/claim that HHEM judges consistent is rescued (dropped / marked supported),
  cutting false positives. Admin-gated by the `groundedness.hhem_filter` runtime knob.
- **HHEM score (consistency)** — `POST {VERIFY_BASE_URL}/v1/hhem`
  `{ "pairs": [["premise", "hypothesis"], …] }` → `{ "scores": [float, …] }` (HHEM-2.1-open,
  `vectara/hallucination_evaluation_model`, Apache-2.0, ~1.2 GB, **lazy-loaded**, needs
  `trust_remote_code`). 1 = fully supported. Used by the benchmark harness.
- **Version** — `GET {VERIFY_BASE_URL}/version` → `{ model, nli_model, factcg_model, hhem_model, method, version }`.

The backend never calls this directly — Rust → ML `/verify` → this engine.

## Dev model / engine

```
pip install -r backend/deploy/verify/requirements.txt
# ~400 MB ModernBERT, downloaded once from Hugging Face, then fully offline:
VERIFY_MODEL=KRLabsOrg/lettucedect-large-modernbert-en-v1 VERIFY_PORT=8095 \
  python backend/deploy/verify/server.py
#   → VERIFY_BASE_URL=http://127.0.0.1:8095  VERIFY_ENABLED=1
```

Port **8095** (8091 reranker, 8092 STT, 8093/8094 TTS are already taken in dev).
CPU is fine — LettuceDetect runs ~30–60 examples/s on GPU and is still well under a
couple of seconds per answer on CPU. The verifier runs **post-stream**, so it never
affects time-to-first-token.

## Smoke

```
curl http://127.0.0.1:8095/version
curl -X POST http://127.0.0.1:8095/v1/verify -H 'content-type: application/json' -d '{
  "context": ["The agreement was signed on 3 March 2021 in London."],
  "question": "When and where was the agreement signed?",
  "answer": "It was signed on 3 March 2021 in Paris, and ran to 400 pages."
}'
# → spans flagging "Paris" and the unsupported "400 pages" claim.
```

Then a manual chat (with `features.groundedness=true` + `VERIFY_*` set, ML service
restarted): ask a question whose answer draws on an attached Project-Knowledge KB →
a groundedness pill appears under the answer; over-reaching claims are listed.

## Benchmark — a measured factual-consistency number (spec §10)

`benchmark.py` runs the **deployed LLM** through the Vectara hallucination-leaderboard
methodology: for each source passage it asks the LLM to summarise, then scores the
`(source, summary)` pair with HHEM via `/v1/hhem`. The mean is a measured, dated
**factual-consistency %** you can put (scoped + dated) in a pitch.

```
# sidecar up + an LLM endpoint reachable:
LLM_BASE_URL=http://localhost:11434/v1 LLM_MODEL=llama3.2 \
VERIFY_BASE_URL=http://127.0.0.1:8095 \
python backend/deploy/verify/benchmark.py --date 2026-06-07
#   → prints "Factual consistency: NN.N%" and writes report.{json,md}
```

`benchmark_data.jsonl` is a tiny bundled **sample** (original passages) so it runs out
of the box — it is **not** the official leaderboard set. For a representative number,
point `LLM_BASE_URL` at the production model and swap in the firm's own evaluation set
(`--data your_set.jsonl`).

**Honest framing (read before the deck, spec §10).** This measures the *served LLM's*
factual consistency on the chosen set — a **different layer** from the span/claim
detector; do not conflate them. The number is only defensible when **measured, scoped,
and dated**. Never market it as "no hallucinations".

## Production

A deployment swaps the verifier via config (`VERIFY_BASE_URL`/`VERIFY_MODEL`) — any
service exposing the contract above is a drop-in. Run it on **GPU2 or CPU**, offline,
pinned to a model + version in the deployment profile. **Excluded** (do not deploy):
Patronus Lynx and Bespoke-MiniCheck (CC-BY-NC, non-commercial) and the **HHEM-2.3
API** (egress). See spec §3 for the licence-clean shortlist (LettuceDetect, FactCG,
HHEM-2.1-open, Granite Guardian).
