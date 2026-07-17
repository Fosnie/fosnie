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

"""Agentic retrieval loop, server-side and invisible to Rust:
decompose → per-sub-question hybrid search → rerank → grade (yes/partial/no) →
reformulate under the round cap → assemble covering context + citations.
Generation is NOT here — it is a separate clean pass after this returns."""

import asyncio
import json
import logging
import re
import time
from collections.abc import Callable
from contextlib import contextmanager
from contextvars import ContextVar
from dataclasses import dataclass, field
from itertools import zip_longest

from prometheus_client import Counter

from . import chunker, embeddings, guided, llm, qdrant_store, reranker, sparse
from .config import settings
from .rag_ctx import cfg
from .web import dedup  # shingle/Jaccard primitives — reused for gap-query near-dup detection

_log = logging.getLogger("pai-ml.retrieve")

# Progress streaming. Same ContextVar shape as web/progress.py: the streaming endpoint
# installs an emitter for its request context; everywhere else `emit()` is a no-op, so the
# non-streaming `/retrieve` path costs nothing. Fire-and-forget — retrieval must NEVER block
# or fail on a slow/gone consumer.
_progress: ContextVar[Callable[[dict], None] | None] = ContextVar("rag_progress_emitter", default=None)


def set_emitter(fn: Callable[[dict], None] | None) -> None:
    """Install the per-request progress emitter (a sync callable taking the event)."""
    _progress.set(fn)


# --- reranker accounting ------------------------------------------------------
# Per-turn rerank tally. A mutable object set in a ContextVar BEFORE the sub-question
# fan-out: asyncio.gather copies the context into each task, but the OBJECT reference
# is shared, so increments from concurrent _search_one calls aggregate back (same
# discipline as _Phases). reranker.rerank() bumps these; retrieve() reads them for the
# INFO summary and the Agent-activity line. `degraded` is any call that fell back.
class _RerankStat:
    def __init__(self) -> None:
        self.calls = 0
        self.degraded = 0

    def record(self, degraded: bool) -> None:
        self.calls += 1
        if degraded:
            self.degraded += 1


_rerank_stat: ContextVar["_RerankStat | None"] = ContextVar("rag_rerank_stat", default=None)


def note_rerank(degraded: bool) -> None:
    """Called by reranker.rerank() to tally one call; no-op outside a retrieve turn."""
    st = _rerank_stat.get()
    if st is not None:
        st.record(degraded)


def emit(stage: str, detail: str | None = None) -> None:
    """Fire a retrieval progress event. Never raises; no-op without an emitter."""
    fn = _progress.get()
    if fn is None:
        return
    event: dict = {"type": "progress", "stage": stage}
    if detail:
        event["detail"] = detail
    try:
        fn(event)
    except Exception as e:  # noqa: BLE001 — progress is best-effort by contract
        _log.debug("rag progress emit failed: %s", e)


# --- per-phase wall-clock timing ----------------------------------------------
# Debug-only instrument to see WHERE a RAG turn spends its time. Same ContextVar
# discipline as `_progress`: `retrieve()` installs an accumulator only when logging
# is at DEBUG, and `_timed` is a no-op otherwise, so production/non-streaming paths
# pay nothing. Sub-questions run concurrently, so phases OVERLAP in wall time — the
# table is per-phase wall, not additive.
class _Phases:
    def __init__(self) -> None:
        self._rows: list[tuple[str, float]] = []

    def add(self, label: str, seconds: float) -> None:
        self._rows.append((label, seconds))

    def summary(self) -> str:
        width = max((len(lbl) for lbl, _ in self._rows), default=5)
        lines = ["(phases overlap under concurrency; per-phase wall, not additive)"]
        total = 0.0
        for lbl, secs in self._rows:
            total += secs
            lines.append(f"  {lbl:<{width}}  {secs * 1000:9.1f} ms")
        lines.append(f"  {'TOTAL (summed)':<{width}}  {total * 1000:9.1f} ms")
        return "\n".join(lines)


_phases: ContextVar["_Phases | None"] = ContextVar("rag_phases", default=None)


@contextmanager
def _timed(label: str):
    """Time a phase into the current request's accumulator; no-op when absent."""
    ph = _phases.get()
    if ph is None:
        yield
        return
    t0 = time.perf_counter()
    try:
        yield
    finally:
        ph.add(label, time.perf_counter() - t0)
# Refire accounting: how often a sub-question reformulates and re-queries, and whether it
# resolved or exhausted the round cap (the latter is wasted latency).
_RAG_REFIRE = Counter("rag_refire_total", "agentic RAG sub-question outcomes", ["outcome"])
# Grade-gate: how often the reranker's own confidence resolved a sub-question
# WITHOUT an LLM grade call (the highest-frequency TTFT saving on good retrieval).
_RAG_GRADE_SKIP = Counter("rag_grade_skip_total", "grade LLM call skipped via rerank confidence")

# Loop system prompts are module constants: they carry NO interpolation, so the small
# per-stage prefixes stay byte-stable and prefix-cacheable across turns. A test guards
# that they remain constant and brace-free — a future edit can't slip a timestamp / id
# in. Keep them free of `{` for that guard.
_DECOMPOSE_SYSTEM = (
    "Decompose the user's question into 1-10 atomic, self-contained sub-questions for "
    "document retrieval. Each sub-question must stand alone with the full context of the "
    "original resolved INTO it — replace pronouns and 'there'/'that'/'it' with the actual "
    "entity (e.g. 'the river there' becomes 'the river that flows through Paris'). Isolate "
    "entities and scenarios: to compare or contrast A and B, or to handle two parties or "
    "situations, emit a SEPARATE sub-question for each — never one blended question. Every "
    "sub-question must be answerable by a document-retrieval system; never ask the user to "
    "clarify. For EACH sub-question also give up to 2 alternative phrasings (synonyms / "
    "expansions) that would retrieve the same facts, and an optional short 'scope' tag "
    "naming the entity, scenario or section it isolates. If the user's question has "
    "NUMBERED parts, for EACH numbered part produce at least one sub-question that "
    "specifically targets it and set 'part' to that part's 1-based index; if the question "
    "has no numbered parts, set 'part' to null. Return ONLY a JSON array of objects: "
    '[{"subq": "...", "queries": ["...", "..."], "scope": "...", "part": 1}]. The first '
    "query of each must be the sub-question itself."
)
# Bounded gap-check run AFTER mini-answers + slice assembly, BEFORE synthesis. Brace-free
# module constant (a test guards the loop prompts stay constant).
_GAP_SYSTEM = (
    "You are checking whether the retrieved evidence is ENOUGH to answer a question part "
    "fully. You are given the part, its sub-answers, and a compact census of the statutory "
    "material already retrieved (section refs + a short preview of each). Decide 'sufficient'. "
    "If it is NOT sufficient, name the SPECIFIC statutory provisions or information still "
    "needed to answer — give explicit section numbers when you know them — as concrete items. "
    "Do not pad: return sufficient=true and an empty list when the evidence already suffices, "
    "and never ask for more than the few items that genuinely matter. Return ONLY a JSON "
    'object: {"sufficient": true|false, "missing": [{"need": "...", "sections": ["571"], '
    '"query": "..."}]}. `query` is a short search phrase for the missing material.'
)
_GRADE_SYSTEM = (
    # Grade SUFFICIENCY, not exhaustiveness. An "does this FULLY answer?" wording makes a rich
    # legal sub-question score 'partial' almost always, burning a wasted reformulation round
    # despite confident retrieval — ask only whether the core material is present.
    "Do the passages contain enough to answer the sub-question — the key rule, section or "
    "facts, even if not every detail? Reply one word: yes (enough to answer), partial (they "
    "touch it but the core material is missing), or no (irrelevant)."
)
_REFORMULATE_SYSTEM = (
    "The passages did not fully answer the sub-question. Write ONE improved "
    "search query as plain text, no quotes."
)
# Isolated per-sub-question answer: answers ONE sub-question from ONLY its own reranked
# passages, so scenario A's chunks can never bleed into B. The NOT-IN-CONTEXT sentinel lets
# the sub-question fail honestly. Module constant, no `{var}` interpolation.
_SUBANSWER_SYSTEM = (
    "Answer ONE sub-question using ONLY the numbered passages provided. Rules: state "
    "only what the passages clearly support; do not assume a rule for one entity, party "
    "or scenario applies to another; do not infer, calculate, or combine facts that are "
    "not explicitly present; answer the sub-question only, treating the broader question "
    "as background. Cite EVERY passage that supports or is relevant to any part of your "
    "answer, even briefly, with its number in square brackets like [1] or [2] — do not "
    "leave a relevant passage uncited. If the passages do not answer the sub-question, "
    "reply with exactly: NOT IN CONTEXT. Be concise."
)
_SUBCHECK_SYSTEM = (
    "Does the answer actually address the sub-question using the passages? Reply one "
    "word: yes or no."
)
_NOT_IN_CONTEXT = "NOT IN CONTEXT"
# Synthesis framing prepended to the returned context (belt-and-suspenders; the
# authoritative, TRUSTED instruction lives in the system prompt — chat/compose.rs —
# because this text sits inside the untrusted retrieved-context fence).
_SYNTH_HEADER = (
    "Below are per-sub-question answers already drafted from the library, then the "
    "consolidated source documents labelled [D1], [D2], and so on. Treat the sub-answers "
    "as an organising scaffold only; the documents are the ground truth. Keep distinct "
    "scenarios, parties and provisions strictly separate — never merge them; where "
    "sub-answers conflict, flag the conflict rather than blending it. Cite every claim "
    "with a document label like [D1]; never cite a sub-answer, and never state a fact "
    "without a [D#] citation. Where a sub-question is marked not found, say so plainly. "
    # (mirror of the TRUSTED instruction in chat/compose.rs):
    "These documents were assembled by verified retrieval, including cross-referenced "
    "statutory sections — do not state that material is absent when a relevant [D#] "
    "exists; consult every [D#] before concluding anything is missing. Quote statutory "
    "language exactly — thresholds, deadlines, sums and section wording — never "
    "paraphrase, and attach a [D#] to every figure, deadline and named threshold."
)

# Citation markers a mini-answer emits over its OWN passage list — parsed to map hits,
# then STRIPPED before the sub-answer enters synthesis (they index a private list).
_CITE_RE = re.compile(r"\[(\d+(?:\s*,\s*\d+)*)\]")


def _dedup_ranked(hits: list[dict], limit: int) -> list[dict]:
    """Distinct child payloads for one sub-question, best rerank first, capped — the
    passages a mini-answer sees and the source of its citable chunks."""
    best: dict[str, tuple[float, dict]] = {}
    for h in hits:
        payload = h["payload"]
        ct = payload["chunk_text"]
        score = h.get("_rerank", 0.0)
        if ct not in best or score > best[ct][0]:
            best[ct] = (score, payload)
    ranked = sorted(best.values(), key=lambda x: x[0], reverse=True)
    return [p for _, p in ranked[: max(1, limit)]]


def _cited_indices(text: str) -> list[int]:
    """1-based passage numbers a mini-answer cited (in first-seen order)."""
    out: list[int] = []
    for m in _CITE_RE.finditer(text):
        for part in m.group(1).split(","):
            part = part.strip()
            if part.isdigit():
                out.append(int(part))
    return out


def _strip_citations(text: str) -> str:
    """Remove [n] / [n, m] markers (strip-and-recite): the sub-answer's numbers
    index its PRIVATE passage list, so the synthesis LLM must re-cite from the unified
    [D#] pool instead of echoing stale local numbers."""
    return re.sub(r"\s{2,}", " ", _CITE_RE.sub("", text)).strip()


def _decompose_objs(out: str) -> list:
    """Pull decomposition objects from the model output. Tries the whole JSON array
    first; if that fails — a long multi-part prompt can TRUNCATE the array at the
    token cap, yielding invalid JSON — salvage every COMPLETE `{...}` object, so we
    still get N sub-questions instead of silently collapsing to a single one."""
    start, end = out.find("["), out.rfind("]")
    if start >= 0 and end > start:
        try:
            arr = json.loads(out[start : end + 1])
            if isinstance(arr, list) and arr:
                return arr
        except ValueError:
            pass
    salvaged: list = []
    for m in re.findall(r"\{[^{}]*\}", out):
        try:
            salvaged.append(json.loads(m))
        except ValueError:
            pass
    return salvaged


# --- decompose coverage-guardrail ---
# A numbered N-part prompt must never lose a whole question. We parse the enumerated
# parts DETERMINISTICALLY, then map sub-questions to parts from the model's own `part`
# tag and — after decompose — inject any UNTAGGED part as a verbatim sub-question
# Belt-and-braces below the LLM: a decompose hiccup or a collapsed multi-part
# prompt can't silently swallow a question, and coverage is asked of the model, not
# guessed from token overlap (which false-positived on a stray shared word).
#
# Line-leading enumerators (dates like `1.10.2007` never match: a marker needs
# `[.)]` followed by WHITESPACE, and the digit run is 1-2 digits): `1.`/`2)`, `(a)`,
# `Q3`/`q3:`. The captured text runs from one marker to the next. This is the PRIORITY
# path; when it finds <2 markers we fall back to inline enumeration below (a single
# paragraph "1. … 2. …").
_PART_MARKER_RE = re.compile(r"(?m)^[ \t]*(?:\(?[a-z]\)|\d{1,2}[.)]|[Qq]\d{1,2}[:.)]?)[ \t]+")
# Inline sequential enumeration: a marker is `<n>[.)]` + whitespace
# + a capitalised / opening-quote start. `(?<![\d.])` rejects decimals and mid-number digits,
# so section refs (`s.549`), sums and years (`2006.` — 4 digits > \d{1,2}) never match. The
# false-positive guard is in _inline_parts: only a gap-free run STARTING AT 1 is accepted.
_INLINE_MARKER_RE = re.compile(r"(?<![\d.])(\d{1,2})[.)]\s+(?=[\"'“«A-ZА-Я])")
_INLINE_CAP = 12  # bound the run (matches max_subqueries_ceiling territory)


def _inline_parts(prompt: str) -> list[str]:
    """Parts of a single-paragraph inline-numbered prompt ("1. … 2. … 3. …"), or [] when
    there is no such run. Only markers whose numbers form a GAP-FREE series from 1 are
    accepted (greedy: take the next marker equal to the expected value) — years, section
    refs and amounts never form 1,2,3,…, so this can't misfire on prose numerals."""
    run: list[tuple[int, int]] = []  # (start, end) of accepted markers, in 1,2,3,… order
    expected = 1
    for m in _INLINE_MARKER_RE.finditer(prompt):
        if int(m.group(1)) == expected:
            run.append((m.start(), m.end()))
            expected += 1
            if expected > _INLINE_CAP:
                break
    if len(run) < 2:
        return []
    parts: list[str] = []
    for i, (_start, mend) in enumerate(run):
        end = run[i + 1][0] if i + 1 < len(run) else len(prompt)
        part = prompt[mend:end].strip()
        if part:
            parts.append(part)
    return parts


def _numbered_parts(prompt: str) -> list[str]:
    """Enumerated parts of a multi-part prompt (text per marker), or [] when the prompt
    isn't clearly enumerated (fewer than 2 markers → treat as one question). Line-start
    enumeration wins; an inline single-paragraph run is the fallback."""
    marks = list(_PART_MARKER_RE.finditer(prompt))
    if len(marks) < 2:
        return _inline_parts(prompt)
    parts: list[str] = []
    for i, m in enumerate(marks):
        end = marks[i + 1].start() if i + 1 < len(marks) else len(prompt)
        part = prompt[m.end():end].strip()
        if part:
            parts.append(part)
    return parts


def _ensure_coverage(prompt: str, items: list[dict]) -> tuple[list[dict], dict]:
    """Guarantee every numbered part of `prompt` has ≥1 sub-question. Coverage is read
    from the model's own 1-based `part` tag — NOT token
    overlap, which false-positived a part as covered on a stray shared word ("company"/
    "shares") while it retrieved nothing. A part with NO tagged sub-question is injected
    VERBATIM as a self-contained sub-question (unconditional — absence of a tag, not weak
    overlap). Post-guardrail every part is tagged-or-injected, so `part_map` covers all
    parts and a per-part slice can never come back empty.
    Returns (items, {parts_detected, parts_covered, subqs_injected, part_map})."""
    parts = _numbered_parts(prompt)
    n_parts = len(parts)
    base_cap = cfg("max_subqueries", settings.max_subqueries)
    if not n_parts:
        # Not enumerated → plain flat cap (unchanged behaviour), no coverage work. No parts
        # ⇒ per_part synthesis falls back to unified (part_map all None).
        kept = items[:base_cap]
        return kept, {"parts_detected": 0, "parts_covered": 0, "subqs_injected": 0,
                      "part_map": [None] * len(kept)}

    ceiling = cfg("max_subqueries_ceiling", settings.max_subqueries_ceiling)

    def _tag(it: dict) -> int | None:
        """The 0-based numbered-part index the model tagged this sub-question with, or None
        (untagged / out of range). bool is an int subclass, so reject it explicitly."""
        p = it.get("part")
        if isinstance(p, int) and not isinstance(p, bool) and 1 <= p <= n_parts:
            return p - 1
        return None

    tagged = [_tag(it) for it in items]
    covered = {t for t in tagged if t is not None}

    # inject every part the model left untagged, VERBATIM, carrying its own tag.
    injected: list[dict] = []
    injected_parts: list[int] = []
    for n, part in enumerate(parts, 1):
        if (n - 1) not in covered:
            injected.append({"subq": part, "queries": [part], "scope": f"part {n}", "part": n})
            injected_parts.append(n - 1)

    # injects are guaranteed and must not squeeze out other parts' sub-questions.
    # Priority order for the LLM items: one per covered part first (a part's only sub-Q is
    # never dropped), then the extras + untagged in original order. Truncating to the
    # remaining budget therefore only ever drops from a part that already has >1 LLM sub-Q.
    budget_llm = max(0, ceiling - len(injected))
    order: list[int] = []
    seen: set[int] = set()
    for idx, t in enumerate(tagged):          # pass 1: secure each covered part's singleton
        if t is not None and t not in seen:
            order.append(idx)
            seen.add(t)
    order += [idx for idx in range(len(items)) if idx not in order]  # pass 2: extras + untagged
    keep = set(order[:budget_llm])
    kept_llm = [items[i] for i in range(len(items)) if i in keep]
    kept_tags = [tagged[i] for i in range(len(items)) if i in keep]

    combined = kept_llm + injected
    part_map = kept_tags + injected_parts
    meta = {
        "parts_detected": n_parts,
        # Tagged-or-injected. By the invariant every part is now one or the other, so this
        # is n_parts — coverage is guaranteed; `subqs_injected` is the honest signal of how
        # many parts the model slept on.
        "parts_covered": len({t for t in part_map if t is not None}),
        "subqs_injected": len(injected),
        # Per-sub-question part index (aligned to `combined`), for per_part synthesis.
        "part_map": part_map,
    }
    return combined, meta


async def _decompose(prompt: str) -> tuple[list[dict], dict]:
    """Decompose into atomic sub-questions AND, for each, ~3 query phrasings (the
    original + reformulations) — one call so the multi-query fan-out costs no extra
    LLM round-trips. The token cap is generous: a 5-part prompt
    expands to many {subq, queries[]} objects, and a truncated array used to fall
    back to a single whole-prompt query (embedding dilution → later questions
    retrieve nothing). Returns (items, coverage-meta); guardrail guarantees every
    numbered part is covered and scales the cap to the part count."""
    llm.set_stage("rag.decompose")
    llm.set_guided(guided.DECOMPOSE)
    try:
        out = await llm.complete(_DECOMPOSE_SYSTEM, prompt, max_tokens=2048)
    except Exception as e:  # noqa: BLE001 — never fail retrieval on a decompose hiccup
        _log.warning("decompose LLM call failed (%s); using whole-prompt query", e)
        # Even on failure the deterministic guardrail salvages a multi-part prompt:
        # inject each numbered part rather than collapse to one diluted whole-prompt query.
        items, meta = _ensure_coverage(prompt, [])
        return (items or [{"subq": prompt, "queries": [prompt], "scope": ""}]), meta
    items: list[dict] = []
    for obj in _decompose_objs(out):
        if not isinstance(obj, dict):
            continue
        subq = str(obj.get("subq", "")).strip()
        if not subq:
            continue
        raw = obj.get("queries", [])
        queries = [q.strip() for q in raw if isinstance(q, str) and q.strip()]
        if subq not in queries:
            queries = [subq, *queries]
        # de-dup, keep order, cap to ~3 variants.
        queries = list(dict.fromkeys(queries))[: cfg("query_variants", settings.query_variants)]
        # Optional scope tag (entity/scenario/section) — labels the synthesis blocks.
        scope = str(obj.get("scope", "")).strip()
        # Optional 1-based numbered-part tag — the model tags
        # which question-part this sub-question serves; range-validated in _ensure_coverage
        # where n_parts is known. bool is an int subclass, so reject it explicitly.
        part = obj.get("part")
        part = part if isinstance(part, int) and not isinstance(part, bool) else None
        items.append({"subq": subq, "queries": queries, "scope": scope, "part": part})
    items = items or [{"subq": prompt, "queries": [prompt], "scope": ""}]
    # guardrail: scale the cap to the part count and inject any uncovered part.
    items, meta = _ensure_coverage(prompt, items)
    _log.info(
        "decompose: planned %d sub-questions (%d/%d parts covered, %d injected; prompt %d chars)",
        len(items), meta["parts_covered"], meta["parts_detected"], meta["subqs_injected"], len(prompt),
    )
    return items, meta


async def _grade(subq: str, chunks: list[str]) -> str:
    if not chunks:
        return "no"
    llm.set_stage("rag.grade")
    llm.set_guided(guided.GRADE)
    out = (
        await llm.complete(
            _GRADE_SYSTEM,
            f"Sub-question: {subq}\n\nPassages:\n" + "\n---\n".join(chunks),
            max_tokens=4,
        )
    ).strip().lower()
    if out.startswith("yes"):
        return "yes"
    if out.startswith("partial"):
        return "partial"
    return "no"


async def _reformulate(subq: str) -> str:
    llm.set_stage("rag.reformulate")
    out = await llm.complete(_REFORMULATE_SYSTEM, f"Sub-question: {subq}", max_tokens=64)
    return out.strip() or subq


def _quote(text: str, max_words: int = 25) -> str:
    """A clean leading quote for a citation chip: start at the first letter/digit
    (skip leading punctuation/whitespace left by chunk overlap) and take up to
    `max_words` words. Verbatim enough that the UI can locate + highlight it."""
    s = text.lstrip()
    # Drop a leading non-alphanumeric run (stray punctuation from a boundary).
    i = 0
    while i < len(s) and not s[i].isalnum():
        i += 1
    return " ".join(s[i:].split()[:max_words])


async def _search_one(
    query: str, kb_ids: list[str], sem: asyncio.Semaphore, *, qdense: list[float] | None = None
) -> list[dict]:
    """One search pipeline (hybrid search → rerank) for a single query, bounded
    by `sem` so concurrent fan-out doesn't flood the backends. The agentic loop
    precomputes `qdense` in one batched `embeddings.embed` call across all
    variants; single-query callers (groundedness
    repair / draft-evidence binding) omit it and it's embedded here. The sparse
    BM25 vector is local CPU so it stays inline. Returns the reranked hits
    (top_k)."""
    async with sem:
        if qdense is None:
            qdense = await embeddings.embed_one(query)
        qsparse = sparse.sparse_one(query)
        hits = await qdrant_store.hybrid_search(kb_ids, qdense, qsparse, cfg("top_k", settings.top_k))
        if not hits:
            return []
        texts = [h["payload"]["chunk_text"] for h in hits]
        scores = await reranker.rerank(query, texts)
        degraded = reranker.degraded_now()
        note_rerank(degraded)
        # reranker-resilience: when the reranker is DOWN it used to
        # return a flat 0.0, silently flattening top-k order AND the grade-gate. Instead
        # fall back to the RRF hybrid-fusion score already on each hit (`hit["score"]`),
        # so ranking + best_rerank stay meaningful — the degradation is surfaced, not muted.
        if degraded:
            for h in hits:
                h["_rerank"] = float(h.get("score", 0.0))
            ranked_hits = sorted(hits, key=lambda h: h["_rerank"], reverse=True)[: cfg("top_k", settings.top_k)]
            return ranked_hits
        ranked = sorted(zip(hits, scores), key=lambda x: x[1], reverse=True)[: cfg("top_k", settings.top_k)]
        # Carry the rerank score on each hit: the grade-gate in _resolve_subq
        # reads it to skip the LLM grade when the cross-encoder is already
        # confident. Rides on the hit, not the payload — downstream reads payload.
        for hit, score in ranked:
            hit["_rerank"] = float(score)
        return [hit for hit, _ in ranked]


async def _resolve_subq(
    item: dict, kb_ids: list[str], sem: asyncio.Semaphore, rounds: int
) -> list[dict]:
    """Resolve one sub-question: fan its ~3 query variants out concurrently,
    grade the merged result, and reformulate for another round (single query)
    until graded `yes` or the round cap is hit. Returns the hits it found."""
    subq = item["subq"]
    queries = item["queries"]
    found: list[dict] = []
    resolved = False
    # Grade-gate threshold (reranker-scale dependent; 0 ⇒ off, always LLM-grade).
    threshold = cfg("grade_skip_threshold", settings.grade_skip_threshold)
    tag = subq[:20]
    for r in range(rounds):
        emit("search", f"searching · round {r + 1}")
        # Embed all variants of this round in ONE request: the embedding
        # server batches them far more efficiently than N separate POSTs, and it
        # cuts connection/queue overhead on the latency-critical path. `embed`
        # preserves order by index, so the vectors zip straight back to queries.
        with _timed(f"{tag} r{r} embed"):
            dense = await embeddings.embed(queries) if queries else []
        with _timed(f"{tag} r{r} search+rerank"):
            results = await asyncio.gather(
                *[_search_one(q, kb_ids, sem, qdense=dv) for q, dv in zip(queries, dense)]
            )
        for hits in results:
            found.extend(hits)
        # Grade against the distinct passages gathered so far.
        texts = list(dict.fromkeys(h["payload"]["chunk_text"] for h in found))[: cfg("top_k", settings.top_k)]
        # When the reranker is already confident (top cross-encoder score clears
        # the threshold), skip the LLM grade CALL — it was scored for free, re-confirming
        # a strong hit with an LLM round-trip is the hottest waste on the TTFT path.
        # the skip must NOT declare coverage achieved — the reranker
        # scale is unbounded and an over-low threshold otherwise silently disables the
        # reformulate round and wrecks recall. So skip the grade call but leave the round
        # loop to run (reformulate if rounds remain); the real coverage signal is the
        # post-retrieval mini-answer, not this score. The score is logged so an
        # operator can calibrate the (scale-dependent) threshold from real traffic.
        best = max((h.get("_rerank", 0.0) for h in found), default=0.0)
        _log.debug("rag grade-gate subq=%.40s best_rerank=%.4f threshold=%s", subq, best, threshold)
        if threshold > 0 and best >= threshold:
            _RAG_GRADE_SKIP.inc()  # saved the grade call; coverage NOT assumed
        else:
            emit("grade", "checking the results")
            with _timed(f"{tag} r{r} grade"):
                graded = await _grade(subq, texts)
            # grade dissection: the VERDICT alongside best_rerank, so an
            # operator can tell mis-calibration (answer present, graded 'no') from weak retrieval.
            _log.debug("rag grade-verdict subq=%.60s round=%d verdict=%s best_rerank=%.4f n_passages=%d",
                       subq, r, graded, best, len(texts))
            # reformulate ONLY when the passages are IRRELEVANT ("no").
            # A "partial" means the topic IS present — on a high-confidence rerank, another
            # round rarely flips it and just burns rerank quota + latency, and the downstream
            # mini-answer is the real coverage signal. So stop on yes OR partial.
            if graded != "no":
                resolved = True
                break
        if r + 1 < rounds:
            emit("search", "refining the search")
            with _timed(f"{tag} r{r} reformulate"):
                queries = [await _reformulate(subq)]
    # Refire accounting (L8): a sub-question that exhausts the cap unresolved is the
    # classic token leak — surface it.
    outcome = "resolved" if resolved else "cap_hit"
    _RAG_REFIRE.labels(outcome=outcome).inc()
    if not resolved:
        _log.info("rag sub-question hit round cap (%s) unresolved: %.80s", rounds, subq)
    return found


# Section / part references pulled from a sub-question to build a BM25-heavy targeted
# query: exact statute numbers are lexical matches — the sparse
# index's strength — so failed sub-answers get one precise re-retrieval before giving up.
_SECTION_REF_RE = re.compile(r"s\.?\s?\d+[A-Za-z]?|section\s+\d+[A-Za-z]?|part\s+\d+[A-Za-z]?", re.IGNORECASE)


def _targeted_query(subq: str) -> str:
    """A lexical-heavy query for the fallback retrieval: the section/part refs found in
    the sub-question (emphasised) plus the sub-question text itself."""
    refs = _SECTION_REF_RE.findall(subq)
    return (" ".join(refs) + " " + subq).strip() if refs else subq


# --- deterministic retrieval expansion ----------------
# Anchor extraction from a sub-question / prompt: single refs (`s443A`, `section 994`,
# `part 15`), ranges (`ss. 570-577`, `sections 570 to 577`), lists (`sections 570 and
# 571`, `570, 571 and 572`) and `Schedule N`. A marker introduces a number EXPRESSION
# that may be a single / range / list — captured whole, then split + range-expanded.
_SEC_MARK = r"(?:ss?\.?|sections?|parts?)"
_NUMEXPR = r"\d+[A-Za-z]?(?:\s*(?:,|and|&|to|through|-|–|—)\s*\d+[A-Za-z]?)*"
_SECTION_LIST_RE = re.compile(rf"\b{_SEC_MARK}\s*\.?\s*({_NUMEXPR})", re.IGNORECASE)
_SCHEDULE_RE = re.compile(r"\bsch(?:edule)?\.?\s*(\d+[A-Za-z]?)", re.IGNORECASE)
_LIST_SPLIT_RE = re.compile(r"\s*(?:,|and|&)\s*", re.IGNORECASE)
_RANGE_RE = re.compile(r"(\d+)\s*(?:to|through|-|–|—)\s*(\d+)$", re.IGNORECASE)


def _fetchable(anchor: str) -> bool:
    """A numeric section id (`443`, `443A`) — the only anchors we can look up
    deterministically (Schedules aren't in the numeric section metadata)."""
    return bool(re.fullmatch(r"\d+[A-Za-z]?", anchor))


def _anchors_from_expr(expr: str) -> set[str]:
    out: set[str] = set()
    for part in _LIST_SPLIT_RE.split(expr):
        part = part.strip()
        if not part:
            continue
        rng = _RANGE_RE.match(part)
        if rng:
            a, b = int(rng.group(1)), int(rng.group(2))
            if a <= b and b - a <= 200:  # sanity bound on a range span
                out.update(str(n) for n in range(a, b + 1))
        else:
            m = re.match(r"\d+[A-Za-z]?", part)
            if m:
                out.add(chunker._norm_ref(m.group(0)))
    return out


def _required_anchors(text: str) -> set[str]:
    """The REQUIRED section set named in `text` (sub-question ∪ prompt): numeric section
    ids + `Schedule N`, normalised (`s. 570 ≡ section 570 ≡ 570.`). Year/date noise dropped."""
    anchors: set[str] = set()
    for m in _SECTION_LIST_RE.finditer(text):
        anchors |= _anchors_from_expr(m.group(1))
    for m in _SCHEDULE_RE.finditer(text):
        anchors.add(f"Schedule {chunker._norm_ref(m.group(1))}")
    return {a for a in anchors if not (a.isdigit() and 1900 <= int(a) <= 2099)}


def _covered_anchors(payloads: list[dict]) -> set[str]:
    """Anchors already present in `payloads` — scanning each chunk's owning section
    (`clause_section_ref`) AND its text (all mentioned refs + Schedules), so a required
    anchor only triggers a look-up when it is genuinely absent."""
    covered: set[str] = set()
    for p in payloads:
        ref = p.get("clause_section_ref")
        if ref:
            covered.add(chunker._norm_ref(ref))
        txt = p.get("chunk_text") or ""
        covered.update(chunker._section_refs(txt))
        for m in _SCHEDULE_RE.finditer(txt):
            covered.add(f"Schedule {chunker._norm_ref(m.group(1))}")
    return covered


class _ExpandStat:
    """Per-turn deterministic-expansion accounting + the shared anchor-lookup budget.
    A mutable object in a ContextVar (survives the `gather` task-context copies, like
    `_RerankStat`); asyncio is cooperative so the sync budget decrement is race-free."""

    def __init__(self, anchor_budget: int) -> None:
        self.anchor_budget = max(0, anchor_budget)
        self.anchors_recovered = 0
        self.crossref_followed = 0
        self.neighbors_fetched = 0
        self.toc_sections_fetched = 0
        # [D#] blocks the late-anchor guardrail added to a part slice.
        self.late_anchor_fetch = 0
        # gap-check→fill accounting (rounds run, gap queries issued, [D#]
        # blocks the fill appended).
        self.gap_rounds = 0
        self.gap_queries = 0
        self.gap_sections_added = 0
        # iterative-retrieval accounting: needs the judge ordered but the corpus
        # could not satisfy (after escalation), and why the loop stopped.
        self.gap_needs_exhausted = 0
        self.gap_stop_reason = ""
        # the exhausted needs' text, surfaced to the backend so the main model's
        # search_library top-up starts from concrete holes rather than from scratch.
        self.gap_unresolved: list[str] = []
        # normalised section refs the deterministic channels FETCHED
        # (crossref/anchor/TOC/neighbours) — lets a retrieval autopsy split a missed expected
        # section into fetched-not-pooled vs not-fetched (which the plain counters only aggregate).
        self.expansion_sections: set[str] = set()

    def take_anchor_budget(self, want: int) -> int:
        n = max(0, min(want, self.anchor_budget))
        self.anchor_budget -= n
        return n


_expand_stat: ContextVar["_ExpandStat | None"] = ContextVar("rag_expand_stat", default=None)


async def _anchor_complete(subq: str, original: str, payloads: list[dict], kb_ids: list[str]) -> list[dict]:
    """anchor-completeness: sections named in the sub-question/prompt (its required set)
    that retrieval missed → deterministic payload look-up, merged in BEFORE the mini-answer,
    regardless of whether the answer would have succeeded. Capped per turn, fail-soft."""
    est = _expand_stat.get()
    if est is None:
        return payloads
    required = _required_anchors(f"{subq}\n{original}")
    if not required:
        return payloads
    missing = sorted(a for a in (required - _covered_anchors(payloads)) if _fetchable(a))
    allow = est.take_anchor_budget(len(missing))
    if allow <= 0:
        return payloads
    try:
        fetched = await qdrant_store.fetch_by_sections(kb_ids, missing[:allow], limit=max(8, allow * 3))
    except Exception as e:  # noqa: BLE001 — expansion is best-effort insurance
        _log.info("anchor lookup failed for %.60s: %s", subq, e)
        return payloads
    seen = {p["chunk_text"] for p in payloads}
    added = [p for p in fetched if p.get("chunk_text") and p["chunk_text"] not in seen]
    est.anchors_recovered += len(added)
    return payloads + added


async def _expand_sections(subq: str, ranked: list[dict], kb_ids: list[str]) -> list[dict]:
    """crossref-follow + neighbours ±N for one sub-question (after its rerank). Follows
    ONE hop: the sections the top chunks cross-reference (`refs_out`) + the sub-question's
    required set, minus what's already covered → operative-text look-up; plus the ±span
    numeric neighbours of the found/required sections. All Qdrant filters, fail-soft, capped."""
    est = _expand_stat.get()
    top = ranked[: cfg("top_k", settings.top_k)]
    refs: set[str] = set()
    nums: set[int] = set()
    for p in top:
        refs.update(p.get("refs_out") or chunker._section_refs(p.get("chunk_text", "")))
        n = chunker._section_num(p.get("clause_section_ref"))
        if n is not None:
            nums.add(n)
    for a in _required_anchors(subq):
        refs.add(a)
        n = chunker._section_num(a)
        if n is not None:
            nums.add(n)
    # STRICT coverage here (only sections whose OPERATIVE text is present, i.e. their own
    # clause_section_ref) — a chunk that merely mentions "see section 570" must still trigger
    # a look-up of s570's operative text. (uses the lenient, mention-inclusive coverage.)
    covered = {chunker._norm_ref(p["clause_section_ref"]) for p in ranked if p.get("clause_section_ref")}
    cap = cfg("crossref_max_sections", settings.crossref_max_sections)
    targets = sorted(r for r in (refs - covered) if _fetchable(r))[:cap]
    span = cfg("neighbor_span", settings.neighbor_span)
    # a TOPICAL sub-question's TOC-swept sections are deterministic expansion (one class with
    # crossref) → lead this sub-question's list so the reserve tier's round-robin pools them first,
    # rather than relying on the mini-answer to cite them.
    out: list[dict] = await _toc_expand(subq, kb_ids)
    try:
        if targets:
            out += await qdrant_store.fetch_by_sections(kb_ids, targets, limit=max(cap, 8))
            if est is not None:
                est.crossref_followed += len(targets)
        if span and nums:
            nb = await qdrant_store.fetch_neighbours(kb_ids, sorted(nums), span, limit=cap * 3)
            out += nb
            if est is not None:
                est.neighbors_fetched += len(nb)
    except Exception as e:  # noqa: BLE001 — expansion is best-effort insurance
        _log.info("section expansion failed for %.60s: %s", subq, e)
    return out


async def _toc_expand(subq: str, kb_ids: list[str]) -> list[dict]:
    """TOC topic→section channel: a TOPICAL sub-question (no numeric
    anchors) — the class the numeric expansion can't reach — is matched by wording against the
    statute chapter titles, then the matched chapter's CONTIGUOUS section-number neighbourhood is
    swept (statute chapters are adjacent, so this reaches a sibling chapter like Pre-emption next
    to Allotment). Returns the payloads (ordered by section_num so the pool's round-robin tiers
    spread across the range) — the caller feeds them to the GUARANTEED reserve pool tier, not the
    mini-answer. Fail-soft, no LLM; inert on a non-statute KB (empty TOC) or a numbered sub-Q."""
    est = _expand_stat.get()
    if _required_anchors(subq):
        return []
    try:
        rows = await qdrant_store.toc_search(kb_ids, subq, limit=2)
    except Exception as e:  # noqa: BLE001 — the TOC channel is best-effort
        _log.info("toc search failed for %.60s: %s", subq, e)
        return []
    sweep = max(1, cfg("toc_max_sections", settings.toc_max_sections))
    # Sweep EACH matched chapter's OWN contiguous range and union them, in BM25-rank order — not
    # `min(los)` over the top matches, which dragged the whole sweep to a far-lower sibling when the
    # two best chapters were distant (e.g. a "pre-emption" match at 560 vs a spurious "Directors'
    # liabilities" at 231). Adjacent statute chapters (Allotment 549 · Pre-emption 560) are still
    # both reached because they surface as separate matches. `_rank` keeps the best chapter's
    # sections at the head so the pool's round-robin tiers pool them first.
    target_nums: list[int] = []
    rank: dict[int, int] = {}
    for r in rows:
        lo = r.get("num_lo")
        if lo is None:
            continue
        for n in range(lo, lo + sweep + 1):
            if n not in rank:
                rank[n] = len(target_nums)
                target_nums.append(n)
    if not target_nums:
        return []
    try:
        hits = await qdrant_store.fetch_neighbours(kb_ids, sorted(rank), 0, limit=len(rank) * 2)
    except Exception as e:  # noqa: BLE001
        _log.info("toc fetch failed for %.60s: %s", subq, e)
        return []
    hits.sort(key=lambda p: (rank.get(p.get("section_num"), 1 << 30), p.get("chunk_index") or 0))
    if est is not None:
        est.toc_sections_fetched += len({p.get("section_num") for p in hits if p.get("section_num") is not None})
    return hits


async def _answer_over(subq: str, original: str, payloads: list[dict], sem: asyncio.Semaphore) -> dict | None:
    """One isolated mini-answer over ONLY `payloads`. Returns {answer, cited} on a real
    answer, or None on empty / NOT-IN-CONTEXT / failed-check (the caller decides whether to
    retry or fail). Runs on the fast utility model at minimal reasoning."""
    if not payloads:
        return None
    passages = "\n\n".join(f"[{i + 1}] {p['chunk_text']}" for i, p in enumerate(payloads))
    user = (
        f"Broader question (background only, do not answer it): {original}\n\n"
        f"Sub-question to answer: {subq}\n\nPassages:\n{passages}"
    )
    timeout = cfg("sub_answer_timeout", settings.sub_answer_timeout)
    try:
        async with sem:
            llm.set_stage("rag.subanswer")
            answer = await asyncio.wait_for(
                llm.complete(
                    _SUBANSWER_SYSTEM,
                    user,
                    max_tokens=cfg("sub_answer_max_tokens", settings.sub_answer_max_tokens),
                ),
                timeout=timeout,
            )
    except Exception as e:  # noqa: BLE001 — fail-soft: a failed sub-answer is reported, not fatal
        _log.info("mini-answer failed for sub-question %.60s: %s", subq, e)
        return None
    answer = (answer or "").strip()
    if not answer or answer.upper().startswith(_NOT_IN_CONTEXT):
        return None
    # Optional cheap yes/no check that the answer addresses the sub-question.
    if cfg("sub_answer_check", settings.sub_answer_check):
        try:
            llm.set_stage("rag.subcheck")
            llm.set_guided(guided.GRADE)
            ok = (
                await llm.complete(_SUBCHECK_SYSTEM, f"Sub-question: {subq}\n\nAnswer: {answer}", max_tokens=4)
            ).strip().lower().startswith("yes")
        except Exception:  # noqa: BLE001 — a failed check must never drop a good answer
            ok = True
        if not ok:
            return None
    # Map the cited [n] back to payloads; if the model cited nothing, keep its top
    # passage so a real answer still contributes a source to the pool.
    cited: list[dict] = []
    seen_local: set[int] = set()
    for n in _cited_indices(answer):
        if 1 <= n <= len(payloads) and n not in seen_local:
            seen_local.add(n)
            cited.append(payloads[n - 1])
    if not cited:
        cited = [payloads[0]]
    return {"answer": answer, "cited": cited}


async def _mini_answer(
    item: dict, hits: list[dict], original: str, sem: asyncio.Semaphore, idx: int, total: int, kb_ids: list[str]
) -> dict:
    """Answer ONE sub-question in an ISOLATED context — only its own reranked passages.
    Fail-soft: a NOT-IN-CONTEXT result triggers ONE
    targeted re-retrieval (section-ref / BM25-heavy) and a re-answer before the honest
    fail. Returns {subq, scope, status, answer, cited, ranked, best_rerank} — `ranked` is
    the full reranked list this answer saw, so the pool can also keep its top uncited
    chunks."""
    subq = item["subq"]
    scope = item.get("scope", "")
    best_rerank = max((h.get("_rerank", 0.0) for h in hits), default=0.0)
    base = {"subq": subq, "scope": scope, "best_rerank": best_rerank}
    n_chunks = cfg("sub_answer_chunks", settings.sub_answer_chunks)
    payloads = _dedup_ranked(hits, n_chunks) if hits else []
    # anchor-completeness: pull any required section the retrieval
    # missed into the mini-answer's context BEFORE it answers — a partial answer must not
    # silently drop a named section. Fail-soft, capped per turn.
    payloads = await _anchor_complete(subq, original, payloads, kb_ids)
    # the TOC topic→section channel now feeds the GUARANTEED reserve pool tier (via
    # `_expand_sections` → crossref_lists), not the mini-answer — so a topical sub-question's
    # sections can't be squeezed out of the [D#] context by the per-sub-Q cited/uncited budget.
    emit("answer", f"answering sub-question {idx + 1} of {total}")
    res = await _answer_over(subq, original, payloads, sem) if payloads else None
    retried = False

    # one targeted fallback on a failed sub-answer — the strongest "need more retrieval"
    # signal (stronger than a rerank score), so it is where the extra latency is spent.
    if res is None and cfg("targeted_fallback_enabled", settings.targeted_fallback_enabled):
        retried = True
        emit("search", f"re-checking the library for sub-question {idx + 1}")
        try:
            rehits = await _search_one(_targeted_query(subq), kb_ids, sem)
        except Exception as e:  # noqa: BLE001 — the retry is best-effort insurance
            _log.info("targeted retry search failed for %.60s: %s", subq, e)
            rehits = []
        if rehits:
            retry_payloads = _dedup_ranked(rehits, n_chunks)
            res = await _answer_over(subq, original, retry_payloads, sem)
            # Keep the retry chunks as `ranked` (they're the freshest, most on-target set);
            # even when the re-answer still fails, these chunks feed the pool.
            payloads = retry_payloads or payloads

    base = {**base, "retried": retried}
    if res is None:
        return {**base, "status": "failed", "answer": "", "cited": [], "ranked": payloads}
    return {**base, "status": "ok", "answer": res["answer"], "cited": res["cited"], "ranked": payloads}


async def _floor_retrieve(prompt: str, kb_ids: list[str], sem: asyncio.Semaphore, n: int) -> list[dict]:
    """Direct retrieval on the ORIGINAL prompt — a floor of chunks that insures the
    pool against a bad decomposition. One search pipeline; insurance,
    so any failure is swallowed."""
    if n <= 0:
        return []
    try:
        hits = await _search_one(prompt, kb_ids, sem)
    except Exception as e:  # noqa: BLE001 — the floor is best-effort insurance
        _log.info("floor retrieve failed: %s", e)
        return []
    return _dedup_ranked(hits, n)


def _citation_of(p: dict) -> dict:
    """Citation record for one pooled chunk — anchored on the child for a precise quote.
    `section_nums` lists EVERY section whose operative text this chunk carries — a chunk that
    swallowed a mid-section heading covers >1 (s563 chunk holds ss563-565), so a consumer can
    attribute the inner section, not just the primary label."""
    return {
        "doc_id": p["doc_id"],
        "chunk_index": p["chunk_index"],
        "page_number": p.get("page_number"),
        "clause_section_ref": p.get("clause_section_ref"),
        "section_nums": p.get("section_nums"),
        "quote_text": _quote(p["chunk_text"]),
    }


def _subanswer_line(i: int, sa: dict) -> str:
    """One labelled, citation-stripped sub-answer scaffold line (shared by unified + per_part).
    A FAILED sub-question whose retrieval IS in the pool points the synthesis LLM at the
    Documents rather than claiming nothing exists; honest 'Not found' stays only when
    retrieval was genuinely empty."""
    scope = f" (scope: {sa['scope']})" if sa.get("scope") else ""
    if sa["status"] == "ok":
        answer = _strip_citations(sa["answer"]) or "See the documents below."
    elif sa.get("ranked"):
        answer = "The sub-search could not produce an answer — check the Documents below for relevant extracts."
    else:
        answer = "Not found in the library."
    return f"Sub-question {i}{scope}: {sa['subq']}\nAnswer: {answer}"


async def _build_synthesis_context(
    sub_results: list[dict], pool: list[dict]
) -> tuple[str, list[dict], int, list[dict]]:
    """Assemble the returned context: the synthesis header, the labelled (citation-stripped)
    sub-answer scaffold, then the consolidated [D#] Documents. Pool
    children are expanded into their full PARENT section (dedup by parent → ONE [D#] per
    parent) so provisos/exceptions living next to a child chunk are physically present —
    mirrors the legacy `_build_context`. Citations anchor on the representative (first,
    highest-priority) child of each block, whose `quote_text` is verbatim inside the block
    (the parent contains the child). Returns (context, citations 1:1 with [D#],
    parents_expanded, doc_meta) — doc_meta[j-1] = {block, subqs} for per_part slicing."""
    # Group the pool by parent (first-seen order); childless chunks stand alone. Collect the
    # SET of contributing sub-questions per block (union across all pooled chunks in the group)
    # so per_part synthesis can hand each part only its own [D#] blocks (turn-global indices).
    order: list = []
    rep: dict = {}
    key_subqs: dict = {}
    for p in pool:
        pid = p.get("parent_id")
        key = pid if pid else ("__child__", p["chunk_text"])
        if key not in rep:
            rep[key] = p
            order.append(key)
            key_subqs[key] = set()
        if "_subq" in p:
            key_subqs[key].add(p["_subq"])

    parent_keys = [k for k in order if not isinstance(k, tuple)]
    max_parents = cfg("max_parents", settings.max_parents)
    ptexts = await qdrant_store.retrieve_parents(parent_keys[:max_parents]) if parent_keys else {}

    doc_blocks: list[str] = []
    citations: list[dict] = []
    doc_meta: list[dict] = []
    parents_expanded = 0
    for j, key in enumerate(order, 1):
        child = rep[key]
        if not isinstance(key, tuple) and key in ptexts:
            text = ptexts[key]
            parents_expanded += 1
        else:
            text = child["chunk_text"]  # childless, or a parent that couldn't be fetched
        block = f"[D{j}] {text}"
        doc_blocks.append(block)
        citations.append(_citation_of(child))
        doc_meta.append({
            "block": block,
            "subqs": key_subqs.get(key, set()),
            "section": child.get("clause_section_ref"),
            # Every section this block OWNS (incl. mid-chunk inner sections) — lets the
            # late-anchor guardrail attribute an owned inner section, not just the label.
            "section_nums": child.get("section_nums"),
        })

    blocks = [_subanswer_line(i, sa) for i, sa in enumerate(sub_results, 1)]

    context = f"{_SYNTH_HEADER}\n\n" + "\n\n".join(blocks) + "\n\nDocuments:\n" + "\n\n".join(doc_blocks)
    return context, citations, parents_expanded, doc_meta


def _build_part_slices(
    prompt: str, sub_results: list[dict], part_map: list[int | None], doc_meta: list[dict]
) -> list[dict]:
    """Per-part synthesis slices. For each numbered part: its OWN
    sub-answers + only the [D#] blocks its sub-questions contributed (referenced by their
    turn-global index, so citations stay 1:1). No cross-part sub-answers → no contamination;
    the original prompt is background only (added backend-side). Empty for a non-numbered
    prompt (≤1 part) → the backend falls back to unified synthesis."""
    parts = _numbered_parts(prompt)
    if len(parts) < 2:
        return []
    slices: list[dict] = []
    for pi, title in enumerate(parts):
        sub_idx = [i for i, m in enumerate(part_map) if m == pi and i < len(sub_results)]
        if not sub_idx:
            continue
        scaffold = [_subanswer_line(i + 1, sub_results[i]) for i in sub_idx]
        sub_set = set(sub_idx)
        member = [dm for dm in doc_meta if dm["subqs"] & sub_set]
        blocks = [dm["block"] for dm in member]
        sections = sorted({dm["section"] for dm in member if dm.get("section")})
        has_evidence = any(sub_results[i]["status"] == "ok" for i in sub_idx) or bool(blocks)
        docs = ("\n\nDocuments:\n" + "\n\n".join(blocks)) if blocks else ""
        context = "\n\n".join(scaffold) + docs
        slices.append({
            "title": title.strip(),
            "context": context,
            "has_evidence": has_evidence,
            # Observability — carried on the slice; the backend
            # SynthPart deserialiser ignores the extra keys.
            "subq_indices": sub_idx,
            "n_blocks": len(blocks),
            "sections": sections,
        })
    return slices


def _best_owning(payloads: list[dict], anchor: str) -> dict | None:
    """Pick the payload that best OWNS `anchor` (exact section match on section_nums or the
    clause label wins; else the first non-empty), or None when the fetch came back empty."""
    m = re.match(r"\d+", anchor)
    num = int(m.group()) if m else None
    fallback: dict | None = None
    key = chunker._norm_ref(anchor)
    for p in payloads:
        if not p.get("chunk_text"):
            continue
        ref = p.get("clause_section_ref")
        if (num is not None and num in set(p.get("section_nums") or [])) or (
            ref and chunker._norm_ref(ref) == key
        ):
            return p
        if fallback is None:
            fallback = p
    return fallback


def _append_slice_block(p: dict, block: str, anchor: str, est: "_ExpandStat | None") -> None:
    """Add one recovered [D#] block to a part slice, under a Documents header if the slice had
    none, and record the recovered section + evidence."""
    sep = "\n\nDocuments:\n" if "Documents:\n" not in p["context"] else "\n\n"
    p["context"] += sep + block
    p["sections"] = sorted(set(p["sections"]) | {anchor})
    p["n_blocks"] += 1
    p["has_evidence"] = True
    if est is not None:
        est.late_anchor_fetch += 1


async def _late_anchor_slices(
    parts: list[dict], citations: list[dict], doc_meta: list[dict], kb_ids: list[str]
) -> None:
    """last guardrail: before per-part synthesis can call a section
    'not reproduced', if that section's NUMBER is NAMED in the part (title / sub-answers /
    slice) but is absent from its slice, recover it deterministically — reuse an already-pooled
    block for pure ATTRIBUTION (also fixes pooled-not-in-slice), else `fetch_by_sections` a
    fresh [D#] and append a 1:1 citation. Mutates `parts` (+ `citations` on a real fetch) in
    place. Cap `late_anchor_cap`/part, fail-soft; inert on a non-numbered turn. A targeted
    fallback on obvious anchors, made deterministic."""
    cap = cfg("late_anchor_cap", settings.late_anchor_cap)
    if cap <= 0 or not parts:
        return
    est = _expand_stat.get()
    # Attribution map: normalised owned section → the [D#] block already in the pool (any part).
    pooled_block: dict[str, str] = {}
    for dm in doc_meta:
        block = dm["block"]
        ref = dm.get("section")
        if ref:
            pooled_block.setdefault(chunker._norm_ref(ref), block)
        for n in (dm.get("section_nums") or []):
            pooled_block.setdefault(chunker._norm_ref(str(n)), block)

    for p in parts:
        try:
            named = {a for a in _required_anchors(f"{p['title']}\n{p['context']}") if _fetchable(a)}
            if not named:
                continue
            present = {chunker._norm_ref(s) for s in p["sections"]}
            for anchor in sorted(named - present)[:cap]:
                key = chunker._norm_ref(anchor)
                blk = pooled_block.get(key)
                if blk is not None:
                    if blk not in p["context"]:  # attribution only — no fetch
                        _append_slice_block(p, blk, anchor, est)
                    continue
                try:
                    fetched = await qdrant_store.fetch_by_sections(kb_ids, [anchor], limit=cap)
                except Exception as e:  # noqa: BLE001 — best-effort insurance
                    _log.info("late-anchor fetch failed for s%s: %s", anchor, e)
                    continue
                pick = _best_owning(fetched, anchor)
                if pick is None:
                    continue  # genuinely absent → leave the honest refusal
                n = len(citations) + 1
                block = f"[D{n}] {pick['chunk_text']}"
                _append_slice_block(p, block, anchor, est)
                citations.append(_citation_of(pick))
                pooled_block[key] = block  # a later part can now attribute, not refetch
        except Exception as e:  # noqa: BLE001 — one part's guardrail never fails the turn
            _log.info("late-anchor guardrail error on part %.40s: %s", p.get("title", ""), e)


# --- bounded LLM gap-check + deterministic fill before synthesis ---------
def _block_head(block: str, n: int = 15) -> str:
    """First ~n words of a `[D#] text…` block, dropping the [D#] token — for a cheap census."""
    return " ".join(block.split()[1 : n + 1])


def _slice_census(part: dict, sub_results: list[dict], doc_meta: list[dict]) -> str:
    """Compact evidence census for the gap-check: the part's sub-answers
    + one short line per in-slice block (section ref + ~15-word preview). Never full texts."""
    sub_idx = [i for i in part.get("subq_indices", []) if i < len(sub_results)]
    scaffold = [_subanswer_line(i + 1, sub_results[i]) for i in sub_idx]
    sub_set = set(sub_idx)
    lines = [f"[{dm.get('section') or '?'}] {_block_head(dm['block'])}"
             for dm in doc_meta if dm["subqs"] & sub_set]
    census = "\n".join(scaffold)
    if lines:
        census += "\n\nEvidence already retrieved:\n" + "\n".join(lines)
    return census


def _unified_census(sub_results: list[dict], doc_meta: list[dict]) -> str:
    """Turn-level census for unified (non-numbered) mode — all sub-answers + block previews."""
    scaffold = [_subanswer_line(i + 1, sa) for i, sa in enumerate(sub_results)]
    lines = [f"[{dm.get('section') or '?'}] {_block_head(dm['block'])}" for dm in doc_meta]
    census = "\n".join(scaffold)
    if lines:
        census += "\n\nEvidence already retrieved:\n" + "\n".join(lines[:40])
    return census


def _parse_gap(out: str) -> dict:
    """Parse the gap-check object; validate + cap missing to 3. Fail-soft to sufficient=true."""
    try:
        s, e = out.find("{"), out.rfind("}")
        if s >= 0 and e > s:
            obj = json.loads(out[s : e + 1])
            if isinstance(obj, dict):
                clean: list[dict] = []
                for m in obj.get("missing") or []:
                    if not isinstance(m, dict):
                        continue
                    need = str(m.get("need", "")).strip()
                    query = str(m.get("query", "")).strip()
                    secs = [str(x).strip() for x in (m.get("sections") or []) if str(x).strip()]
                    if need or query or secs:
                        clean.append({"need": need, "query": query or need, "sections": secs})
                # Any named gap ⇒ insufficient, regardless of the model's own flag.
                return {"sufficient": bool(obj.get("sufficient", True)) and not clean,
                        "missing": clean[:3]}
    except (ValueError, TypeError):
        pass
    return {"sufficient": True, "missing": []}


async def _gap_check(title: str, census: str) -> dict:
    """One bounded gap-check LLM call (guided JSON, minimal reasoning, ~15s), fail-soft."""
    llm.set_stage("rag.gapcheck")
    llm.set_guided(guided.GAP)
    try:
        out = await asyncio.wait_for(
            llm.complete(_GAP_SYSTEM, f"{title}\n\n{census}", max_tokens=512), timeout=15
        )
    except Exception as e:  # noqa: BLE001 — a gap-check hiccup never fails retrieval
        _log.info("gap-check failed for %.40s: %s", title, e)
        return {"sufficient": True, "missing": []}
    return _parse_gap(out)


async def _gap_toc(text: str, kb_ids: list[str]) -> list[dict]:
    """TOC title-match for a gap item: chapter → contiguous section sweep (the _toc_expand idiom)."""
    rows = await qdrant_store.toc_search(kb_ids, text, limit=2)
    if not rows:
        return []
    sweep = max(1, cfg("toc_max_sections", settings.toc_max_sections))
    nums: list[int] = []
    for row in rows:
        lo = row.get("num_lo")
        if lo is not None:
            nums.extend(range(lo, lo + sweep + 1))
    if not nums:
        return []
    return await qdrant_store.fetch_neighbours(kb_ids, sorted(set(nums)), 0, limit=len(nums))


async def _gap_fetch(item: dict, kb_ids: list[str], sem: asyncio.Semaphore) -> list[dict]:
    """Deterministic fill for one missing item: fetch_by_sections + a
    BM25-heavy _search_one + a TOC title match, run concurrently, each fail-soft. Payload dicts."""
    est = _expand_stat.get()
    if est is not None:
        est.gap_queries += 1
    secs = [chunker._norm_ref(s) for s in item.get("sections", [])]
    secs = [s for s in secs if _fetchable(s)]
    query = (item.get("query") or item.get("need") or "").strip()
    toc_text = (item.get("need") or query).strip()

    async def _by_sections() -> list[dict]:
        return await qdrant_store.fetch_by_sections(kb_ids, secs, limit=12) if secs else []

    async def _by_query() -> list[dict]:
        if not query:
            return []
        hits = await _search_one(_targeted_query(query), kb_ids, sem)
        return _dedup_ranked(hits, cfg("top_k", settings.top_k))

    async def _by_toc() -> list[dict]:
        return await _gap_toc(toc_text, kb_ids) if toc_text else []

    results = await asyncio.gather(_by_sections(), _by_query(), _by_toc(), return_exceptions=True)
    payloads: list[dict] = []
    for r in results:
        if not isinstance(r, Exception) and r:
            payloads += r
    return payloads


async def _expand_gap_blocks(payloads: list[dict], start_idx: int) -> list[tuple[str, dict]]:
    """Parent-expand gap payloads into (block, child) with [D#] labels from `start_idx` — one
    [D#] per distinct parent/child, mirroring _build_synthesis_context's parent dedup."""
    order: list = []
    rep: dict = {}
    for p in payloads:
        pid = p.get("parent_id")
        key = pid if pid else ("__child__", p["chunk_text"])
        if key not in rep:
            rep[key] = p
            order.append(key)
    parent_keys = [k for k in order if not isinstance(k, tuple)]
    ptexts = await qdrant_store.retrieve_parents(parent_keys) if parent_keys else {}
    out: list[tuple[str, dict]] = []
    n = start_idx
    for key in order:
        child = rep[key]
        text = ptexts.get(key, child["chunk_text"]) if not isinstance(key, tuple) else child["chunk_text"]
        out.append((f"[D{n}] {text}", child))
        n += 1
    return out


def _append_gap_block(p: dict, block: str, sec: str | None, est: "_ExpandStat | None") -> None:
    """Append a recovered gap [D#] block to a part slice (mirror _append_slice_block, gap counter)."""
    sep = "\n\nDocuments:\n" if "Documents:\n" not in p["context"] else "\n\n"
    p["context"] += sep + block
    if sec:
        p["sections"] = sorted(set(p["sections"]) | {sec})
    p["n_blocks"] += 1
    p["has_evidence"] = True
    if est is not None:
        est.gap_sections_added += 1


# --- iterative-retrieval controller (generalises the single gap round) --------
# Bounded evidence-sufficiency loop: keep gap-checking + filling across rounds until the
# evidence suffices OR the corpus is exhausted, then hand the honest shortfall to synthesis.
# Budget discipline (deadline / reserve / diminishing returns / anti-thrash) mirrors the golden
# web/loop.py `_State`; the pattern is COPIED here (per the freeze on web/loop.py + DR), not shared.


@dataclass
class _GapState:
    """Mutable loop budget + cross-round memory for the iterative-retrieval controller."""

    deadline: float                                  # time.monotonic() deadline (+inf = off)
    reserve_left: int                                # [D#] blocks the fill may still append
    round: int = 0
    stop_reason: str = ""                            # sufficient|rounds|reserve|deadline|…
    seen_ct: set[str] = field(default_factory=set)   # chunk_texts seen across the WHOLE loop
    query_history: list[str] = field(default_factory=list)  # normalised gap queries already tried
    # per missing-item memory between rounds: key → {item, attempts, exhausted, target}.
    needs: dict[str, dict] = field(default_factory=dict)

    def expired(self) -> bool:
        return time.monotonic() >= self.deadline


def _norm_query(q: str) -> str:
    """Lower-case + collapse whitespace — the normal form for query-repeat detection."""
    return " ".join((q or "").lower().split())


def _need_key(item: dict) -> str:
    """Stable key for a missing-item across rounds: normalised need + sorted sections."""
    secs = ",".join(sorted(str(s).strip() for s in (item.get("sections") or []) if str(s).strip()))
    return f"{_norm_query(item.get('need') or item.get('query') or '')}|{secs}"


def _is_repeat_query(q: str, history: list[str]) -> bool:
    """A gap query is a repeat when it exactly matches, or token-Jaccard ≥ 0.8 overlaps, a
    prior round's query (reuses web/dedup shingles — no embeddings, cheap). Guards thrash."""
    nq = _norm_query(q)
    if not nq:
        return False
    if nq in history:
        return True
    qs = dedup.shingles(nq)
    if not qs:
        return False
    return any(dedup.jaccard(qs, dedup.shingles(h)) >= 0.8 for h in history)


def _need_synopsis(orders: list[tuple], limit: int = 60) -> str:
    """Short human synopsis of a round's ordered needs, for the progress label."""
    needs = [(o[1].get("need") or o[1].get("query") or "").strip() for o in orders]
    text = "; ".join(n for n in needs if n)[:limit]
    return text or "additional provisions"


def _record_gap_meta(doc_meta: list[dict], block: str, sec: str | None, child: dict, subqs: set) -> None:
    """CENSUS-FIX: register a freshly-added gap [D#] block in `doc_meta` so the NEXT round's
    census (_slice_census / _unified_census read doc_meta) reflects what this round already
    fetched — otherwise the judge re-orders material it was just handed."""
    doc_meta.append({
        "block": block,
        "subqs": subqs,  # part's sub-question indices (per-part) or ∅ (unified shows all)
        "section": sec or child.get("clause_section_ref"),
        "section_nums": child.get("section_nums"),
    })


async def _gap_escalate_fetch(item: dict, kb_ids: list[str], sem: asyncio.Semaphore) -> list[dict]:
    """Escalation pass for an item a first fetch could not satisfy — makes round N+1 ≠ round N:
    a FULL hybrid search over the `need` text (no _targeted_query wrapper), one _reformulate of
    the query, and a +1 neighbour span over the item's sections. Wider net, still bounded
    (top_k×2), all concurrent + fail-soft, all through qdrant_store so the ACL deny-list holds."""
    est = _expand_stat.get()
    if est is not None:
        est.gap_queries += 1
    need = (item.get("need") or item.get("query") or "").strip()
    query = (item.get("query") or need).strip()
    secs = [chunker._norm_ref(s) for s in item.get("sections", [])]
    secs = [s for s in secs if _fetchable(s)]
    top_k2 = max(1, cfg("top_k", settings.top_k) * 2)  # bounded — do not inflate rerank

    async def _hybrid_need() -> list[dict]:
        if not need:
            return []
        hits = await _search_one(need, kb_ids, sem)  # raw need text, not the judge's search phrase
        return _dedup_ranked(hits, top_k2)

    async def _reformulated() -> list[dict]:
        try:
            rq = await _reformulate(query)
        except Exception:  # noqa: BLE001 — reformulation is best-effort
            return []
        if not rq or _norm_query(rq) == _norm_query(query):
            return []
        hits = await _search_one(rq, kb_ids, sem)
        return _dedup_ranked(hits, top_k2)

    async def _neighbours() -> list[dict]:
        nums: list[int] = []
        for s in secs:
            m = re.match(r"\d+", s)
            if m:
                nums.append(int(m.group()))
        if not nums:
            return []
        return await qdrant_store.fetch_neighbours(kb_ids, sorted(set(nums)), 1, limit=len(nums) * 3 or 12)

    results = await asyncio.gather(_hybrid_need(), _reformulated(), _neighbours(), return_exceptions=True)
    payloads: list[dict] = []
    for r in results:
        if not isinstance(r, Exception) and r:
            payloads += r
    return payloads


def _finalise_known_gaps(st: "_GapState", context: str, est: "_ExpandStat | None") -> str:
    """Honest insufficiency: every need still `exhausted` (ordered by the judge, not found
    after escalation) becomes a neutral service block INSIDE the untrusted context — so synthesis
    names the shortfall rather than looping or inventing. Returns the updated unified context."""
    buckets: list[tuple] = []  # [(target, [need, …])]  — target is a part dict or None (unified)
    for rec in st.needs.values():
        if not rec.get("exhausted"):
            continue
        need = (rec["item"].get("need") or rec["item"].get("query") or "").strip()
        if not need:
            continue
        if est is not None:
            est.gap_needs_exhausted += 1
            est.gap_unresolved.append(need)
        t = rec.get("target")
        for bt, needs in buckets:
            if bt is t:
                needs.append(need)
                break
        else:
            buckets.append((t, [need]))
    for t, needs in buckets:
        line = "Not found in the library after exhaustive search: " + "; ".join(needs)
        if t is not None:
            t["context"] += "\n\n" + line
        else:
            context += "\n\n" + line
    return context


async def _gap_round(
    prompt: str, parts: list[dict], sub_results: list[dict], context: str,
    citations: list[dict], doc_meta: list[dict], pool: list[dict],
    kb_ids: list[str], sem: asyncio.Semaphore,
) -> str:
    """Iterative retrieval: a bounded evidence-sufficiency loop. Each round gap-checks every
    target (per part, or per turn in unified mode), deterministically fills what the judge names
    into an append-only non-evictable budget, ESCALATES items a prior round could not satisfy,
    and stops on sufficiency or corpus exhaustion — then records honest known-gaps. Runs
    BEFORE _late_anchor_slices (which stays the final guardrail). Mutates `parts`/`citations`/
    `doc_meta` in place; returns the (possibly-updated) unified context. Fail-soft throughout —
    any error is at worst an early stop, never a failed retrieval."""
    if not cfg("gap_round_enabled", settings.gap_round_enabled):
        return context
    reserve = max(0, cfg("gap_reserve", settings.gap_reserve))
    rounds = max(0, cfg("gap_rounds", settings.gap_rounds))
    if reserve <= 0 or rounds <= 0:
        return context
    est = _expand_stat.get()
    escalate = bool(cfg("gap_escalate", settings.gap_escalate))
    dim = max(0.0, min(1.0, float(cfg("gap_diminishing_unseen", settings.gap_diminishing_unseen))))
    deadline_secs = max(0.0, float(cfg("gap_deadline_secs", settings.gap_deadline_secs)))
    st = _GapState(
        deadline=(time.monotonic() + deadline_secs) if deadline_secs > 0 else float("inf"),
        reserve_left=reserve,
        seen_ct={p["chunk_text"] for p in pool if p.get("chunk_text")},
    )
    targets = parts if parts else [None]  # None ⇒ the unified pseudo-target

    for _r in range(rounds):
        st.round += 1
        if st.reserve_left <= 0:
            st.stop_reason = "reserve"
            break
        if st.expired():
            st.stop_reason = "deadline"
            break
        # 1. Census is kept current by the fills below (doc_meta append) — the judge on round r+1
        #    sees what round r already brought, so it does not re-order it.
        censuses = [
            (t, (t["title"] if t is not None else prompt),
             (_slice_census(t, sub_results, doc_meta) if t is not None
              else _unified_census(sub_results, doc_meta)))
            for t in targets
        ]
        try:
            checks = await asyncio.gather(*[_gap_check(title, cen) for _, title, cen in censuses])
        except Exception as e:  # noqa: BLE001 — a whole-round gap-check hiccup only stops early
            _log.info("gap-check round failed: %s", e)
            st.stop_reason = "no_added"
            break
        if est is not None:
            est.gap_rounds += 1
        # (a) all targets sufficient ⇒ success.
        if all(chk["sufficient"] for chk in checks):
            st.stop_reason = "sufficient"
            break
        # 3. Dedup the orders against cross-round memory: skip exhausted items (in census yet still
        #    asked ⇒ not in corpus), escalate second-attempts, close thrash after two tries.
        orders: list[tuple] = []  # (target, item, escalate_this_item)
        for (t, _, _), chk in zip(censuses, checks):
            if chk["sufficient"]:
                continue
            for item in chk["missing"]:
                key = _need_key(item)
                rec = st.needs.get(key)
                q = item.get("query") or item.get("need") or ""
                repeat_q = _is_repeat_query(q, st.query_history)
                if rec is None:
                    st.needs[key] = {"item": item, "attempts": 0, "exhausted": False,
                                     "found_fresh": False, "target": t}
                    orders.append((t, item, escalate and repeat_q))
                elif rec.get("exhausted"):
                    continue  # judge saw it in the census and still asks ⇒ the corpus lacks it
                elif rec["attempts"] >= 2:
                    # Tried + escalated already ⇒ close it (anti-thrash cap: two attempts, never a
                    # third). Only a KNOWN GAP if it never yielded fresh evidence; an item that did
                    # is resolved (already in the census), just not re-ordered.
                    rec["exhausted"] = not rec.get("found_fresh", False)
                else:
                    orders.append((t, item, escalate))  # second attempt ⇒ escalation pass
        # (f) nothing left to order (everything is a repeat of an exhausted item).
        if not orders:
            st.stop_reason = "no_pending"
            break
        emit("gapfill", f"Round {st.round}: searching for {_need_synopsis(orders)}")
        round_fetched = 0
        round_unseen = 0
        added = 0
        for t, item, esc in orders:
            if st.reserve_left <= 0 or st.expired():
                break
            key = _need_key(item)
            rec = st.needs.setdefault(
                key, {"item": item, "attempts": 0, "exhausted": False, "found_fresh": False, "target": t})
            rec["attempts"] += 1
            nq = _norm_query(item.get("query") or item.get("need") or "")
            if nq:
                st.query_history.append(nq)
            try:
                payloads = await (_gap_escalate_fetch(item, kb_ids, sem) if esc
                                  else _gap_fetch(item, kb_ids, sem))
            except Exception as e:  # noqa: BLE001 — fill is best-effort insurance
                _log.info("gap fetch failed: %s", e)
                if rec["attempts"] >= 2 and not rec.get("found_fresh"):
                    rec["exhausted"] = True
                continue
            fetched = [p for p in payloads if p.get("chunk_text")]
            round_fetched += len(fetched)
            fresh = [p for p in fetched if p["chunk_text"] not in st.seen_ct]
            round_unseen += len(fresh)
            if not fresh:
                if rec["attempts"] >= 2 and not rec.get("found_fresh"):  # escalation also empty ⇒ genuinely absent
                    rec["exhausted"] = True
                continue
            rec["found_fresh"] = True
            blocks = await _expand_gap_blocks(fresh[: st.reserve_left], len(citations) + 1)
            for block, child in blocks:
                if st.reserve_left <= 0:
                    break
                st.seen_ct.add(child["chunk_text"])
                citations.append(_citation_of(child))
                sec = child.get("clause_section_ref") or (item["sections"][0] if item.get("sections") else None)
                if t is not None:
                    _append_gap_block(t, block, sec, est)
                    _record_gap_meta(doc_meta, block, sec, child, set(t.get("subq_indices", [])))
                else:
                    context = _append_unified_block(context, block)
                    if est is not None:
                        est.gap_sections_added += 1
                    _record_gap_meta(doc_meta, block, sec, child, set())
                st.reserve_left -= 1
                added += 1
        # (e) diminishing returns: a round that fetched material but almost none of it NEW is
        #     spinning — stop (this also covers "fetched only already-seen chunks", ratio 0).
        #     Empty-fetch rounds (round_fetched == 0) are NOT diminishing: they let an item reach
        #     its escalation pass, after which exhaustion drains it to the (f) no_pending stop.
        if dim > 0 and round_fetched > 0 and (round_unseen / max(1, round_fetched)) < dim:
            st.stop_reason = "diminishing"
            break
    if not st.stop_reason:
        st.stop_reason = "rounds"  # (b) hit the round cap
    if est is not None:
        est.gap_stop_reason = st.stop_reason
    return _finalise_known_gaps(st, context, est)


def _append_unified_block(context: str, block: str) -> str:
    """Append a gap [D#] block to the unified context string (under a Documents header)."""
    sep = "\n\nDocuments:\n" if "Documents:\n" not in context else "\n\n"
    return context + sep + block


def _round_robin_payloads(per_subq: list[list[dict]], max_chunks: int) -> list[dict]:
    """ROUND-ROBIN merge of sub-question hits into distinct payloads (the pre-
    behaviour; also the pool fallback when every mini-answer failed). Take each
    sub-question's top hit, then each one's 2nd, so a strong early sub-question can't
    hog the budget and starve later ones."""
    collected: dict[str, dict] = {}
    for tier in zip_longest(*per_subq):
        for hit in tier:
            if hit is not None:
                collected.setdefault(hit["payload"]["chunk_text"], hit["payload"])
        if len(collected) >= max_chunks:
            break
    return list(collected.values())[:max_chunks]


async def _legacy_assemble(per_subq: list[list[dict]]) -> dict:
    """Pre- path (sub_answer_enabled=False, or the no-LLM degradation): merge, cite
    (deduped, capped) and build the plain covering context with parent expansion."""
    max_chunks = cfg("max_context_chunks", settings.max_context_chunks)
    payloads = _round_robin_payloads(per_subq, max_chunks)
    _CITATION_CAP = 8
    citations: list[dict] = []
    seen: set[tuple] = set()
    for p in payloads:
        key = (p["doc_id"], p.get("page_number"), p.get("clause_section_ref"))
        if key in seen:
            continue
        seen.add(key)
        citations.append(_citation_of(p))
        if len(citations) >= _CITATION_CAP:
            break
    emit("assemble", "assembling context")
    with _timed("assemble+parents"):
        context = await _build_context(payloads)
    return {"context": context, "citations": citations}


def _assemble_pool(
    sub_results: list[dict],
    crossref_lists: list[list[dict]],
    floor_payloads: list[dict],
    per_subq: list[list[dict]],
) -> tuple[list[dict], list[int], int]:
    """Consolidate the [D#] pool by ROUND-ROBIN TIERS.
    Order: cited → CROSS-REFERENCED → uncited → floor. Cross-referenced operative-text
    chunks (required anchors, refs_out, ±N neighbours) draw from a SEPARATE reserve ON TOP of
    `pool_budget`, and run BEFORE generic uncited — so a precisely fetched section can never be
    evicted by an uncited chunk (the "followed but not pooled" bug). Stamps `p["_subq"]` for
    per_part attribution. Returns (pool, contrib, cross_used) — cross_used = cross-ref chunks
    that ACTUALLY pooled (compare with crossref_followed to see the old squeeze)."""
    n_subq = len(sub_results)
    per_budget = cfg("pool_per_subq_budget", settings.pool_per_subq_budget)
    uncited_cap = cfg("pool_uncited_per_subq", settings.pool_uncited_per_subq)
    cited_cap = max(1, per_budget - uncited_cap)
    pool_budget = min(
        cfg("pool_hard_cap", settings.pool_hard_cap),
        max(cfg("max_context_chunks", settings.max_context_chunks), n_subq * per_budget),
    )
    crossref_reserve = max(0, cfg("pool_crossref_reserve", settings.pool_crossref_reserve))
    hard_ceiling = pool_budget + crossref_reserve

    # autopsy telemetry: every section the deterministic channels FETCHED (whether or not
    # it survives the reserve below) — so a loss-table can tell fetched-not-pooled from
    # not-fetched per section. Cheap: one pass over the already-materialised crossref lists.
    _est = _expand_stat.get()
    if _est is not None:
        for _lst in crossref_lists:
            for _p in _lst:
                _ref = _p.get("clause_section_ref")
                if _ref:
                    _est.expansion_sections.add(chunker._norm_ref(_ref))
                for _n in (_p.get("section_nums") or []):
                    _est.expansion_sections.add(chunker._norm_ref(str(_n)))

    pool: list[dict] = []
    seen_ct: set[str] = set()
    contrib = [0] * n_subq  # per-sub-Q chunks that made the pool (observability)
    budget_used = 0         # cited + uncited + floor, bounded by pool_budget
    cross_used = 0          # cross-referenced, bounded by the separate reserve

    def _add(p: dict, i: int | None, tier: str) -> None:
        nonlocal budget_used, cross_used
        ct = p["chunk_text"]
        if ct in seen_ct or len(pool) >= hard_ceiling:
            return
        if tier == "cross":
            if cross_used >= crossref_reserve:
                return
        elif budget_used >= pool_budget:
            return
        seen_ct.add(ct)
        # Stamp the contributing sub-question (per_part); floor chunks (i=None) are
        # prompt-level insurance, attributed to no part.
        if i is not None:
            p["_subq"] = i
            contrib[i] += 1
        pool.append(p)
        if tier == "cross":
            cross_used += 1
        else:
            budget_used += 1

    # Per-sub-Q contribution lists: OK → cited (capped) + top uncited from ranked; FAILED with
    # retrieval → its ranked treated as uncited but capped at `uncited_cap`
    # (a sub-Q that couldn't produce an answer no longer takes the full per-Q budget).
    cited_lists: list[list[dict]] = []
    uncited_lists: list[list[dict]] = []
    for sa in sub_results:
        ranked = sa.get("ranked", [])
        cited = sa.get("cited", [])
        cited_texts = {p["chunk_text"] for p in cited}
        uncited = [p for p in ranked if p["chunk_text"] not in cited_texts]
        if sa["status"] == "ok":
            cited_lists.append(cited[:cited_cap])
            uncited_lists.append(uncited[:uncited_cap])
        else:
            cited_lists.append([])
            uncited_lists.append(ranked[:uncited_cap])

    # Phase A — cited tiers (round-robin, budget).
    for k in range(cited_cap):
        for i in range(n_subq):
            if k < len(cited_lists[i]):
                _add(cited_lists[i][k], i, "budget")
    # Phase B2 — cross-referenced / neighbour sections BEFORE generic uncited, and on the
    # separate reserve, so they're never squeezed out: an edge provision that was fetched
    # deterministically gets pooled, not merely followed.
    for k in range(max((len(c) for c in crossref_lists), default=0)):
        for i in range(n_subq):
            if k < len(crossref_lists[i]):
                _add(crossref_lists[i][k], i, "cross")
    # Phase B — uncited tiers (OK + failed-with-ranked), budget.
    for k in range(max((len(u) for u in uncited_lists), default=0)):
        for i in range(n_subq):
            if k < len(uncited_lists[i]):
                _add(uncited_lists[i][k], i, "budget")
    # Phase C — the direct floor, budget.
    for p in floor_payloads:
        _add(p, None, "budget")
    # Total washout (no sub-answer had retrieval AND no floor): round-robin the raw hits so the
    # turn still has grounded context rather than an empty pool.
    if not pool:
        pool = _round_robin_payloads(per_subq, pool_budget)
    return pool, contrib, cross_used


async def retrieve(prompt: str, kb_ids: list[str], deny_doc_ids: list[str] | None = None) -> dict:
    # Fail-closed (Libraries): an empty allow-list ⇒ ZERO retrieval. The
    # backend resolves the allow-list (attached ∩ can-read); we never "search all".
    if not kb_ids:
        return {"context": "", "citations": []}

    # Source-ACL retrieval deny-list. Installed on the shared
    # ContextVar BEFORE any fan-out so every concurrent query task inherits it and
    # honours the `must_not doc_id` filter — a document the caller's connected-source
    # ACL excludes must surface through no retrieval path. Empty ⇒ no filtering.
    qdrant_store.set_deny_docs(deny_doc_ids)

    # install a per-turn phase timer only at DEBUG (no cost otherwise).
    if _log.isEnabledFor(logging.DEBUG):
        _phases.set(_Phases())

    # rerank accounting: a mutable per-turn counter, installed BEFORE the fan-out
    # so it survives asyncio task-context copies (same trick as _Phases). reranker.rerank
    # increments it; the summary + activity line read it.
    rstat = _RerankStat()
    _rerank_stat.set(rstat)
    # Deterministic-expansion accounting + shared anchor budget,
    # installed before the fan-out so concurrent mini-answers share one per-turn budget.
    estat = _ExpandStat(cfg("anchor_lookup_max", settings.anchor_lookup_max))
    _expand_stat.set(estat)

    emit("decompose", "understanding your question")
    with _timed("decompose"):
        decomposed, cover_meta = await _decompose(prompt)
    emit("decompose", f"Breaking the question into {len(decomposed)} sub-questions…")
    # Hard ceiling on rounds regardless of how high max_rounds is tuned (DoS bound).
    rounds = max(1, min(cfg("max_rounds", settings.max_rounds), settings.max_rounds_ceiling))
    sem = asyncio.Semaphore(max(1, settings.retrieve_concurrency))

    # Sub-questions resolve concurrently (each fans its variants out concurrently);
    # the semaphore caps total in-flight search pipelines.
    per_subq = await asyncio.gather(
        *[_resolve_subq(item, kb_ids, sem, rounds) for item in decomposed]
    )

    # Pre- path: merge every sub-question's chunks into ONE shared context and answer
    # once. Kept as the toggle-off / no-LLM degradation path — isolation needs a working
    # generation model, so a bare-metal local box without one still returns context.
    if not cfg("sub_answer_enabled", settings.sub_answer_enabled):
        result = await _legacy_assemble(per_subq)
        ph = _phases.get()
        if ph is not None:
            _log.debug("rag phase summary\n%s", ph.summary())
        return result

    # ISOLATION: answer each sub-question in its own context (parallel, fail-soft) and,
    # concurrently, retrieve a direct floor on the original prompt (insurance against a bad
    # decomposition). Then synthesise from the labelled sub-answers over ONE [D#] pool.
    total = len(decomposed)
    with _timed("mini-answers"):
        sub_results, floor_payloads = await asyncio.gather(
            asyncio.gather(
                *[
                    _mini_answer(item, hits, prompt, sem, i, total, kb_ids)
                    for i, (item, hits) in enumerate(zip(decomposed, per_subq))
                ]
            ),
            _floor_retrieve(prompt, kb_ids, sem, cfg("pool_floor_chunks", settings.pool_floor_chunks)),
        )

    # deterministic expansion: after each sub-question's rerank,
    # follow its top chunks' cross-references + pull ±N neighbours (pure Qdrant filters,
    # concurrent, fail-soft). Feeds a dedicated "cross-referenced" pool tier below.
    with _timed("expand"):
        crossref_lists = await asyncio.gather(
            *[
                _expand_sections(decomposed[i]["subq"], sub_results[i].get("ranked", []), kb_ids)
                for i in range(len(sub_results))
            ]
        )

    # Consolidated [D#] pool: round-robin tiers
    # cited → cross-referenced (reserved) → uncited → floor, so precisely-fetched statutory
    # sections can never be squeezed out by generic uncited chunks.
    n_subq = len(sub_results)
    pool, contrib, cross_used = _assemble_pool(sub_results, crossref_lists, floor_payloads, per_subq)

    emit("assemble", f"All sources gathered — synthesising the answer from {len(pool)} sources…")
    # Expand [D#] blocks into full parent sections, 1:1 citations after parent-dedup.
    context, citations, parents_expanded, doc_meta = await _build_synthesis_context(sub_results, pool)
    # per_part: deterministic per-part slices over the turn-global [D#] pool (empty for a
    # non-numbered prompt → backend uses unified). The backend chooses which mode to render.
    parts = _build_part_slices(prompt, sub_results, cover_meta.get("part_map", []), doc_meta)
    # agentic gap-fill — the LLM checks each part's evidence and names what's
    # still missing; a deterministic fill tops up the slice from a non-evictable append budget.
    # BEFORE _late_anchor_slices so late-anchor stays the last deterministic guardrail.
    context = await _gap_round(prompt, parts, sub_results, context, citations, doc_meta, pool, kb_ids, sem)
    # last guardrail: recover a section a part NAMES but whose slice
    # lacks it, BEFORE the per-part synthesis can wrongly call it "not reproduced". Mutates
    # parts + citations in place; fail-soft; inert on a non-numbered turn.
    await _late_anchor_slices(parts, citations, doc_meta, kb_ids)

    n_ok = sum(1 for sa in sub_results if sa["status"] == "ok")
    n_sources = len({c["doc_id"] for c in citations})
    n_sections = len({c["clause_section_ref"] for c in citations if c.get("clause_section_ref")})
    # crossref_pooled = cross-ref chunks that ACTUALLY entered the
    # pool. Divergence from crossref_followed is the "followed but not pooled" regression —
    # keep both visible every turn.
    n_parts_ev = sum(1 for p in parts if p["has_evidence"])
    _log.info(
        "rag turn subqs=%d ok=%d failed=%d pool=%d parents=%d blocks=%d "
        "parts=%d/%d injected=%d rerank_calls=%d degraded=%d "
        "anchors_recovered=%d crossref_followed=%d crossref_pooled=%d neighbors_fetched=%d toc_fetched=%d "
        "late_anchor=%d gap_rounds=%d gap_queries=%d gap_added=%d gap_unresolved=%d gap_stop=%s",
        n_subq, n_ok, n_subq - n_ok, len(pool), parents_expanded, len(citations),
        cover_meta["parts_covered"], cover_meta["parts_detected"], cover_meta["subqs_injected"],
        rstat.calls, rstat.degraded,
        estat.anchors_recovered, estat.crossref_followed, cross_used, estat.neighbors_fetched,
        estat.toc_sections_fetched, estat.late_anchor_fetch,
        estat.gap_rounds, estat.gap_queries, estat.gap_sections_added,
        estat.gap_needs_exhausted, estat.gap_stop_reason or "-",
    )
    # (+): a product-facing, always-on summary STEP
    # in the chat Agent activity panel — the acceptance signals live here, and it doubles as
    # a "show your work" trust cue for legal users. British English; no debug jargon.
    emit("summary", _activity_summary(cover_meta, n_subq, n_ok, n_sources, n_sections, rstat, estat,
                                      n_parts_ev, len(parts)))
    debug = {
        "pool_total": len(pool),
        "parents_expanded": parents_expanded,
        "blocks": len(citations),
        "parts_detected": cover_meta["parts_detected"],
        "parts_covered": cover_meta["parts_covered"],
        "subqs_injected": cover_meta["subqs_injected"],
        "rerank_calls": rstat.calls,
        "rerank_degraded": rstat.degraded,
        "anchors_recovered": estat.anchors_recovered,
        "crossref_followed": estat.crossref_followed,
        "crossref_pooled": cross_used,
        "neighbors_fetched": estat.neighbors_fetched,
        "toc_sections_fetched": estat.toc_sections_fetched,
        # + autopsy: late-anchor recoveries this turn, and the set of
        # sections the deterministic channels fetched (to split fetched-not-pooled vs not-fetched).
        "late_anchor_fetch": estat.late_anchor_fetch,
        "expansion_sections": sorted(estat.expansion_sections),
        # iterative-retrieval accounting this turn.
        "gap_rounds": estat.gap_rounds,
        "gap_queries": estat.gap_queries,
        "gap_sections_added": estat.gap_sections_added,
        "gap_needs_exhausted": estat.gap_needs_exhausted,
        "gap_stop_reason": estat.gap_stop_reason,
        "gap_unresolved": estat.gap_unresolved,
        # per-part slice view (context stripped) — settle "which
        # sections landed in a given part's slice" in one run.
        "parts": [
            {"title": p["title"], "subq_indices": p["subq_indices"], "n_blocks": p["n_blocks"],
             "sections": p["sections"], "has_evidence": p["has_evidence"]}
            for p in parts
        ],
        "sub_questions": [
            {"subq": sa["subq"][:80], "scope": sa.get("scope", ""), "status": sa["status"],
             "best_rerank": round(sa["best_rerank"], 4), "pool_contrib": contrib[i],
             "retried": sa.get("retried", False)}
            for i, sa in enumerate(sub_results)
        ],
    }
    ph = _phases.get()
    if ph is not None:
        _log.debug("rag phase summary\n%s", ph.summary())
    return {"context": context, "citations": citations, "debug": debug, "parts": parts}


def _plural(n: int, noun: str) -> str:
    return f"{n} {noun}" if n == 1 else f"{n} {noun}s"


def _activity_summary(
    cover_meta: dict, n_subq: int, n_ok: int, n_sources: int, n_sections: int,
    rstat: "_RerankStat", estat: "_ExpandStat | None" = None,
    n_parts_ev: int = 0, n_parts: int = 0,
) -> str:
    """Human-readable retrieval summary for the Agent activity panel. Example:
    'Coverage: 5/5 parts · 8 sub-questions (1 not found) · 28 documents, 14 sections ·
    reranker OK · anchors recovered: 3 · cross-references followed: 5'. Degradation,
    guardrail recovery and deterministic expansion are surfaced, never hidden."""
    bits: list[str] = []
    if cover_meta["parts_detected"]:
        bits.append(f"Coverage: {cover_meta['parts_covered']}/{cover_meta['parts_detected']} parts")
    not_found = n_subq - n_ok
    subq_bit = _plural(n_subq, "sub-question")
    if not_found:
        subq_bit += f" ({not_found} not found)"
    bits.append(subq_bit)
    bits.append(f"{_plural(n_sources, 'document')}, {_plural(n_sections, 'section')}")
    bits.append("reranker degraded (using hybrid scores)" if rstat.degraded else "reranker OK")
    if cover_meta["subqs_injected"]:
        bits.append(f"{_plural(cover_meta['subqs_injected'], 'part')} recovered")
    if estat is not None and estat.anchors_recovered:
        bits.append(f"anchors recovered: {estat.anchors_recovered}")
    if estat is not None and estat.crossref_followed:
        bits.append(f"cross-references followed: {estat.crossref_followed}")
    if estat is not None and estat.late_anchor_fetch:
        bits.append(f"sections recovered by reference: {estat.late_anchor_fetch}")
    if estat is not None and (estat.gap_sections_added or estat.gap_needs_exhausted):
        bit = f"gap-fill: {estat.gap_sections_added} sections"
        if estat.gap_needs_exhausted:
            bit += f", {estat.gap_needs_exhausted} needs unresolved"
        bits.append(bit)
    # on a multi-part prompt, how many parts have retrieved evidence
    # backing them — the trust signal for per_part synthesis.
    if n_parts:
        bits.append(f"parts with evidence: {n_parts_ev}/{n_parts}")
    return " · ".join(bits)


async def _build_context(payloads: list[dict]) -> str:
    """L2: when children carry `parent_id`, hand the LLM the enclosing parent
    sections (distinct, first-seen order), not the bare children. Otherwise the
    per-child context. Falls back to children if parents can't be fetched."""
    parent_ids: list[str] = []
    for p in payloads:
        pid = p.get("parent_id")
        if pid and pid not in parent_ids:
            parent_ids.append(pid)

    max_parents = cfg("max_parents", settings.max_parents)
    if parent_ids:
        texts = await qdrant_store.retrieve_parents(parent_ids[:max_parents])
        blocks = [texts[pid] for pid in parent_ids[:max_parents] if pid in texts]
        if blocks:
            return "\n\n".join(f"[{i + 1}] {t}" for i, t in enumerate(blocks))

    return "\n\n".join(f"[{i + 1}] {p['chunk_text']}" for i, p in enumerate(payloads))
