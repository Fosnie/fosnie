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

"""Exhaustive agentic web-search loop, server-
side and invisible to Rust and the LLM — the web analogue of retrieve.py:

  plan (decompose → sub-questions × engine-shaped variants + freshness intent)
  → per sub-question, per round:
       SERP fan-out → snippet pool → composite URL ranking → fetch top-N
       → extract/chunk/date → syndication dedup → rerank chunks → grade
       → reformulate (narrow) for another round
  → stop on: graded yes | round cap | diminishing returns | global budget
  → per sub-question conflict check → one triggered verification round
  → beast-mode assembly (always returns a best-effort digest; never raises).

`quick` skips planning and runs a single round (≈ the Phase-1 pipeline). The
budgets scale by depth; `deep` is the widest and runs as a background job."""

import asyncio
import json
import logging
import time
from dataclasses import dataclass
from datetime import date

from .. import guided, llm, reranker
from ..config import settings
from . import dedup, pipeline, progress, rank
from .pipeline import _Source, _domain, _domain_allowed, _fetch_source, _quote, _serp
from .provider import SerpResult

_log = logging.getLogger("pai.web.loop")

_EVIDENCE_PER_SUBQ = 8     # top reranked chunks kept per sub-question
_MAX_SNIPPET_SOURCES = 40  # cap on snippet-only sources accumulated in the pool
_DIMINISHING_UNSEEN = 0.2  # a round adding < this fraction unseen URLs → stop


# --- Budget ------------------------------------------------------------------

@dataclass
class Budget:
    decompose: bool
    subqs: int
    rounds: int
    serp_limit: int        # SERP results considered per query
    fetch_per_round: int   # pages fetched per sub-question per round
    max_serp_queries: int  # global cap across the whole run
    max_fetches: int       # global cap across the whole run
    wall_clock: float      # seconds; on expiry → beast-mode assembly


def _budget(depth: str) -> Budget:
    d = (depth or "standard").lower()
    ceiling = settings.max_rounds_ceiling
    if d == "quick":
        return Budget(False, 1, 1, 5, 2, 4, 3, settings.web_wall_clock_quick)
    if d == "deep":
        return Budget(
            True, 4, min(settings.web_deep_rounds, ceiling), 15, 6, 48, 28,
            settings.web_wall_clock_deep,
        )
    return Budget(
        True, 3, min(settings.web_max_rounds, ceiling), 10, 4, 12, 8,
        settings.web_wall_clock_standard,
    )


@dataclass
class _State:
    """Mutable global budget counters + deadline, shared across sub-questions."""

    serp_budget: int
    fetch_budget: int
    deadline: float
    beast: bool = False  # set when a budget/deadline cut the work short

    def expired(self) -> bool:
        return time.monotonic() >= self.deadline


@dataclass
class _SubQ:
    text: str
    queries: list[str]
    freshness: str
    current_query: str = ""

    def __post_init__(self):
        if not self.current_query:
            self.current_query = self.text


# --- Source pool -------------------------------------------------------------

class _Pool:
    """Accumulates web sources. Snippet-only sources enter immediately (cheap
    evidence); a later fetch of the same URL upgrades it in place."""

    def __init__(self) -> None:
        self.sources: list[_Source] = []
        self._by_url: dict[str, int] = {}
        self.shingles: list[set[str]] = []
        self._snippet_count = 0

    def add_snippet(self, r: SerpResult) -> None:
        if r.url in self._by_url or self._snippet_count >= _MAX_SNIPPET_SOURCES:
            return
        self._by_url[r.url] = len(self.sources)
        self.sources.append(
            _Source(
                url=r.url,
                title=r.title or _domain(r.url),
                domain=_domain(r.url),
                published_date=r.published_date,
                fetched_at=_now(),
                snippet_only=True,
                chunks=[r.snippet] if r.snippet else [],
            )
        )
        self._snippet_count += 1

    def upgrade_fetched(self, src: _Source) -> int:
        """Insert/replace a fetched source; returns its pool index."""
        if src.url in self._by_url:
            idx = self._by_url[src.url]
            self.sources[idx] = src
        else:
            idx = len(self.sources)
            self._by_url[src.url] = idx
            self.sources.append(src)
        return idx


def _now() -> str:
    from datetime import datetime, timezone

    return datetime.now(timezone.utc).isoformat(timespec="seconds")


# --- LLM steps (network-safe: degrade rather than raise) ---------------------

async def _plan(query: str, recency: str, today: str, max_subqs: int) -> list[_SubQ]:
    """One LLM call → sub-questions × engine-shaped variants + freshness intent.
    Mirrors retrieve._decompose's JSON-extraction-with-fallback; any failure
    (including the LLM being unreachable) degrades to a single sub-question."""
    system = (
        f"Today is {today}. Decompose the user's question into 1-{max_subqs} atomic "
        "sub-questions for web search. For EACH, give up to 2 alternative search "
        "queries phrased as a search engine expects (keywords, not prose), and a "
        '"freshness" hint (one of: any, year, month, week, day) for how recent the '
        "answer must be. Prefer BROAD queries first — over-specified queries are the "
        "dominant failure mode; narrowing happens in later rounds. Return ONLY a JSON "
        'array: [{"subq": "...", "queries": ["...", "..."], "freshness": "any"}]. The '
        "first query of each must be the sub-question itself."
    )
    try:
        llm.set_stage("web.plan")
        llm.set_guided(guided.WEB_PLAN)
        out = await llm.complete(system, query, max_tokens=512)
    except Exception as e:  # noqa: BLE001 — degrade to single sub-question
        _log.warning("web plan LLM unavailable, single sub-question: %s", e)
        return [_SubQ(query, [query], recency)]
    try:
        start, end = out.find("["), out.rfind("]")
        arr = json.loads(out[start : end + 1]) if start >= 0 else []
        subqs: list[_SubQ] = []
        for obj in arr:
            if not isinstance(obj, dict):
                continue
            text = str(obj.get("subq", "")).strip()
            if not text:
                continue
            raw = obj.get("queries", [])
            queries = [q.strip() for q in raw if isinstance(q, str) and q.strip()]
            if text not in queries:
                queries = [text, *queries]
            queries = list(dict.fromkeys(queries))[: settings.query_variants]
            fresh = str(obj.get("freshness", "any")).strip().lower()
            if fresh not in {"any", "year", "month", "week", "day"}:
                fresh = "any"
            subqs.append(_SubQ(text, queries, fresh))
        return subqs[:max_subqs] or [_SubQ(query, [query], recency)]
    except (ValueError, TypeError):
        return [_SubQ(query, [query], recency)]


async def _grade(subq: str, evidence_lines: list[str], today: str) -> str:
    """yes/partial/no sufficiency grade. Evidence lines carry publish dates so the
    grader can weight freshness. LLM unavailable → 'yes' (stop, don't loop on a
    broken grader and burn the SERP budget)."""
    if not evidence_lines:
        return "no"
    try:
        llm.set_stage("web.grade")
        llm.set_guided(guided.GRADE)
        out = (
            await llm.complete(
                f"Today is {today}. Do the passages fully and currently answer the "
                "sub-question? Prefer recent sources when the question implies freshness. "
                "Reply one word: yes, partial, or no.",
                f"Sub-question: {subq}\n\nPassages:\n" + "\n---\n".join(evidence_lines),
                max_tokens=4,
            )
        ).strip().lower()
    except Exception as e:  # noqa: BLE001
        _log.warning("web grade LLM unavailable, stopping rounds: %s", e)
        return "yes"
    if out.startswith("yes"):
        return "yes"
    if out.startswith("partial"):
        return "partial"
    return "no"


async def _reformulate(subq: str) -> str:
    try:
        llm.set_stage("web.reformulate")
        out = await llm.complete(
            "The passages did not fully answer the sub-question. Write ONE improved, "
            "NARROWER web search query (add one distinguishing term). Plain text, no quotes.",
            f"Sub-question: {subq}",
            max_tokens=64,
        )
    except Exception as e:  # noqa: BLE001
        _log.warning("web reformulate LLM unavailable: %s", e)
        return subq
    return out.strip() or subq


async def _detect_conflict(subq: str, evidence_lines: list[str]) -> tuple[bool, str]:
    """One LLM call → do the sources materially disagree? Failure → no conflict."""
    if len(evidence_lines) < 2:
        return (False, "")
    try:
        llm.set_stage("web.detect_conflict")
        llm.set_guided(guided.WEB_CONFLICT)
        out = await llm.complete(
            "Do the sources materially disagree on the answer to the sub-question? "
            'Return ONLY JSON {"conflict": true|false, "topic": "<what they disagree on>"}.',
            f"Sub-question: {subq}\n\nSources:\n" + "\n---\n".join(evidence_lines),
            max_tokens=80,
        )
        start, end = out.find("{"), out.rfind("}")
        obj = json.loads(out[start : end + 1]) if start >= 0 else {}
        return (bool(obj.get("conflict", False)), str(obj.get("topic", "")).strip())
    except Exception as e:  # noqa: BLE001
        _log.debug("web conflict check failed (treating as no conflict): %s", e)
        return (False, "")


# --- Ranking + fetch helpers -------------------------------------------------

async def _rank_candidates(
    subq: str, candidates: list[tuple[SerpResult, int]]
) -> list[SerpResult]:
    """Composite-rank candidate (result, frequency) pairs for one sub-question."""
    if not candidates:
        return []
    texts = [f"{r.title} — {r.snippet}" for r, _ in candidates]
    rr = await reranker.rerank(subq, texts)
    rr_norm = rank.normalise(rr)
    scored = [
        (rank.composite(rr_norm[i], freq, _domain(r.url), r.url), r)
        for i, (r, freq) in enumerate(candidates)
    ]
    scored.sort(key=lambda x: x[0], reverse=True)
    return [r for _, r in scored]


async def _fetch_batch(
    results: list[SerpResult], pool: _Pool, state: _State
) -> list[int]:
    """Fetch a batch (paced, SSRF-guarded), drop near-duplicates, fold survivors
    into the pool. Returns the pool indices of the fetched, non-duplicate pages."""
    sem = asyncio.Semaphore(max(1, settings.web_fetch_concurrency))
    fetched = await asyncio.gather(*[_fetch_source(r, sem) for r in results])
    idxs: list[int] = []
    for src in fetched:
        if src.snippet_only:
            # Fetch failed → the snippet is still in the pool; nothing to fold.
            pool.upgrade_fetched(src)
            continue
        lead = src.chunks[0] if src.chunks else ""
        if dedup.is_near_duplicate(lead, pool.shingles, settings.web_dedup_threshold):
            _log.debug("dropping near-duplicate %s", src.url)
            continue
        pool.shingles.append(dedup.shingles(lead))
        idxs.append(pool.upgrade_fetched(src))
    return idxs


def _evidence_lines(pool: _Pool, idxs: list[int]) -> list[str]:
    """Grade/conflict-facing evidence: domain + date + lead chunk per source."""
    lines: list[str] = []
    for i in idxs:
        s = pool.sources[i]
        meta = f"[{s.domain}{', ' + s.published_date if s.published_date else ''}]"
        if s.chunks:
            lines.append(f"{meta} {s.chunks[0]}")
    return lines


# --- Per-sub-question resolution ---------------------------------------------

async def _resolve_subq(
    sq: _SubQ, recency: str, budget: Budget, state: _State, pool: _Pool, seen: set[str]
) -> list[int]:
    """Run the round loop for one sub-question; return the pool indices of its
    fetched evidence (used for conflict detection)."""
    recency_eff = recency if recency != "any" else sq.freshness
    candidates: dict[str, tuple[SerpResult, int]] = {}  # url -> (result, freq)
    evidence_idxs: list[int] = []

    for r in range(budget.rounds):
        if state.expired() or state.serp_budget <= 0:
            state.beast = True
            break

        queries = sq.queries if r == 0 else [sq.current_query]
        queries = queries[: max(1, state.serp_budget)]
        state.serp_budget -= len(queries)
        progress.emit("serp", "; ".join(queries), round=r + 1, subq=sq.text)
        serp_lists = await asyncio.gather(
            *[_serp(q, recency_eff, budget.serp_limit) for q in queries]
        )

        round_urls: set[str] = set()
        new_unseen = 0
        for lst in serp_lists:
            for res in lst:
                if not _domain_allowed(res.url):
                    continue
                round_urls.add(res.url)
                if res.url in candidates:
                    candidates[res.url] = (candidates[res.url][0], candidates[res.url][1] + 1)
                else:
                    candidates[res.url] = (res, 1)
                    pool.add_snippet(res)
                    if res.url not in seen:
                        new_unseen += 1

        # Diminishing returns: this round surfaced almost nothing new.
        unseen_ratio = new_unseen / max(1, len(round_urls))

        # Rank all accumulated candidates; fetch the top unseen within budget.
        ranked = await _rank_candidates(sq.text, list(candidates.values()))
        to_fetch: list[SerpResult] = []
        for res in ranked:
            if res.url in seen:
                continue
            if len(to_fetch) >= budget.fetch_per_round or state.fetch_budget <= 0:
                break
            to_fetch.append(res)
            seen.add(res.url)
            state.fetch_budget -= 1

        if to_fetch:
            progress.emit(
                "fetch", ", ".join(dict.fromkeys(_domain(x.url) for x in to_fetch)),
                round=r + 1, subq=sq.text,
            )
        fetched_idxs = await _fetch_batch(to_fetch, pool, state) if to_fetch else []
        evidence_idxs.extend(fetched_idxs)

        # Diminishing returns: nothing new fetched and the SERPs are going stale.
        if not fetched_idxs and unseen_ratio < _DIMINISHING_UNSEEN:
            break
        # On the final round grading/reformulation can change nothing — skip the
        # LLM calls (this is what makes the quick path zero-LLM).
        if r + 1 >= budget.rounds:
            break
        graded_idxs = await _topk_evidence(sq.text, pool, evidence_idxs)
        verdict = await _grade(sq.text, _evidence_lines(pool, graded_idxs), _today())
        progress.emit("grade", verdict, round=r + 1, subq=sq.text)
        if verdict == "yes":
            break
        sq.current_query = await _reformulate(sq.text)
        progress.emit("reformulate", sq.current_query, round=r + 1, subq=sq.text)

    return await _topk_evidence(sq.text, pool, evidence_idxs)


async def _topk_evidence(subq: str, pool: _Pool, idxs: list[int]) -> list[int]:
    """Rerank the fetched sources' lead chunks for a sub-question; return the
    top source indices (dedup-stable)."""
    uniq = list(dict.fromkeys(idxs))
    if len(uniq) <= _EVIDENCE_PER_SUBQ:
        return uniq
    leads = [pool.sources[i].chunks[0] if pool.sources[i].chunks else "" for i in uniq]
    scores = await reranker.rerank(subq, leads)
    ranked = [i for i, _ in sorted(zip(uniq, scores), key=lambda x: x[1], reverse=True)]
    return ranked[:_EVIDENCE_PER_SUBQ]


def _today() -> str:
    return date.today().isoformat()


# --- Orchestrator ------------------------------------------------------------

@dataclass
class CollectResult:
    """The gathering phase's full output — richer than the digest `run()`
    assembles from it. Deep Research consumes this directly (full `_Source`
    objects with chunks) while `run()` keeps its Phase-1/2 digest contract."""

    pool: _Pool
    subq_evidence: list  # list[tuple[_SubQ, list[int]]]
    notes: list[str]
    beast: bool


async def collect(
    query: str,
    recency: str,
    budget: Budget,
    *,
    pool: _Pool | None = None,
    seen: set[str] | None = None,
    state: _State | None = None,
) -> CollectResult:
    """The evidence-gathering phase: plan (when the budget decomposes) →
    per-sub-question rounds (SERP fan-out → ranking → paced fetch → dedup →
    grade → reformulate) → conflict verification. Callers may share `pool`,
    `seen` and `state` across calls (Deep Research merges several collects
    into one pool under one global budget); `run()` passes none and gets the
    original single-call behaviour."""
    pool = pool if pool is not None else _Pool()
    seen = seen if seen is not None else set()
    if state is None:
        state = _State(
            serp_budget=budget.max_serp_queries,
            fetch_budget=budget.max_fetches,
            deadline=time.monotonic() + budget.wall_clock,
        )
    today = _today()
    notes: list[str] = []

    if budget.decompose:
        progress.emit("plan", "decomposing the question")
        subqs = await _plan(query, recency, today, budget.subqs)
        progress.emit("plan", f"{len(subqs)} sub-question{'s' if len(subqs) != 1 else ''}")
    else:
        fresh = recency if recency != "any" else "any"
        subqs = [_SubQ(query, [query], fresh)]

    # Sub-questions resolve sequentially so the global SERP/fetch budget stays
    # race-free; variant fan-out within a round is concurrent.
    subq_evidence: list[tuple[_SubQ, list[int]]] = []
    for sq in subqs:
        if state.expired():
            state.beast = True
            break
        idxs = await _resolve_subq(sq, recency, budget, state, pool, seen)
        subq_evidence.append((sq, idxs))

    # Conflict detection per sub-question → one triggered verification round.
    # Skipped on the quick path (decompose off) to keep it cheap and LLM-free.
    for sq, idxs in subq_evidence if budget.decompose else []:
        if state.expired() or state.serp_budget <= 0 or state.fetch_budget <= 0:
            break
        conflict, topic = await _detect_conflict(sq.text, _evidence_lines(pool, idxs))
        if not conflict:
            continue
        progress.emit("conflict", topic or sq.text, subq=sq.text)
        notes.append(
            f"⚠ Sources disagree on {topic or sq.text} — both positions are "
            "included below; weigh them and say so in the answer."
        )
        existing_domains = {pool.sources[i].domain for i in idxs}
        state.serp_budget -= 1
        extra = await _serp(f"{sq.text} {topic}".strip(), "any", budget.serp_limit)
        extra = [r for r in extra if _domain_allowed(r.url) and r.url not in seen]
        # Prefer independent domains for the verification.
        extra.sort(key=lambda r: _domain(r.url) in existing_domains)
        pick: list[SerpResult] = []
        for r in extra:
            if len(pick) >= 2 or state.fetch_budget <= 0:
                break
            pick.append(r)
            seen.add(r.url)
            state.fetch_budget -= 1
        idxs.extend(await _fetch_batch(pick, pool, state))

    return CollectResult(pool=pool, subq_evidence=subq_evidence, notes=notes, beast=state.beast)


async def run(query: str, recency: str = "any", depth: str = "standard") -> dict:
    from ..rag_ctx import cfg

    budget = _budget(depth)
    # Per-Agent fetch cap (sent by the backend as a request override) min-clamps
    # the depth class's budget — an agent can tighten, never widen.
    agent_max_fetches = cfg("web_max_fetches", None)
    if isinstance(agent_max_fetches, int) and agent_max_fetches > 0:
        budget.max_fetches = min(budget.max_fetches, agent_max_fetches)

    collected = await collect(query, recency, budget)
    pool = collected.pool
    subq_evidence = collected.subq_evidence
    notes = collected.notes

    if collected.beast:
        notes.append(
            "Note: the search budget was exhausted; this digest is best-effort from "
            "the evidence gathered so far."
        )

    # Global assembly: every sub-question's evidence chunks, deduped by text,
    # reranked once against the original query, capped.
    progress.emit("assemble", "ranking and assembling the digest")
    cap = 14 if depth == "deep" else pipeline._MAX_DIGEST_CHUNKS
    candidate_pairs: list[tuple[int, str]] = []
    seen_chunks: set[str] = set()
    for _sq, idxs in subq_evidence:
        for i in idxs:
            for chunk in pool.sources[i].chunks:
                c = chunk.strip()
                if c and c not in seen_chunks:
                    seen_chunks.add(c)
                    candidate_pairs.append((i, chunk))
    # Fold in snippet-only sources as cheap fallback evidence.
    for i, s in enumerate(pool.sources):
        if s.snippet_only:
            for chunk in s.chunks:
                c = chunk.strip()
                if c and c not in seen_chunks:
                    seen_chunks.add(c)
                    candidate_pairs.append((i, chunk))

    if not candidate_pairs:
        digest = (
            "No usable web results were found for this query (search engines may be "
            "unavailable). Tell the user web search returned nothing rather than guessing."
        )
        if notes:
            digest = "\n".join(notes) + "\n\n" + digest
        return {"digest": digest, "citations": []}

    scores = await reranker.rerank(query, [c for _, c in candidate_pairs])
    picked = [
        pair for pair, _ in sorted(zip(candidate_pairs, scores), key=lambda x: x[1], reverse=True)
    ][:cap]

    assembled = pipeline._assemble(query, pool.sources, picked, notes=notes)

    # Deep runs in the background and posts its answer DIRECTLY into the chat (no
    # chat-turn LLM to synthesise it, unlike the inline quick/standard path). So
    # here we turn the assembled evidence into a streamed prose answer with [n]
    # citations. Quick/standard return the raw evidence digest unchanged — their
    # answer is written (and streamed) by the chat turn that called the tool.
    # Synthesise only when a token consumer is listening (the streaming deep path);
    # without one (non-streaming callers, tests that stub complete) keep the original
    # assembled-digest behaviour and make no extra LLM call.
    if (
        (depth or "standard").lower() == "deep"
        and assembled.get("citations")
        and progress.token_emitter_installed()
    ):
        progress.emit("write", "writing the answer")
        answer = await _synthesise_deep(query, assembled["digest"])
        if answer.strip():
            return {"digest": answer, "citations": assembled["citations"]}
    return assembled


# Generous ceiling for the deep synthesis answer (the digest can be long; the
# background path has no tool timeout pressure).
_SYNTH_MAX_TOKENS = 2048


async def _synthesise_deep(query: str, evidence: str) -> str:
    """Stream a prose answer to `query` grounded ONLY in `evidence` (the assembled
    digest, whose sources are numbered [n]). Tokens stream live via
    `progress.emit_token`; the full text is returned for persistence. Degrades to
    the raw evidence digest on any failure — never raises."""
    system = (
        "You are a research assistant. Write a thorough, well-structured answer to "
        "the user's question using ONLY the evidence provided.\n"
        "Hard rules:\n"
        "- Cite sources inline with their numeric markers like [1] or [3], using ONLY "
        "the numbers present in the evidence. NEVER invent URLs, titles or sources.\n"
        "- Do not output a 'Web sources:' list or a reference list — the citations "
        "are attached separately.\n"
        "- If the evidence is insufficient for part of the question, say so plainly "
        "rather than guessing."
    )
    user = f"Question: {query}\n\nEvidence:\n{evidence}"
    acc: list[str] = []
    try:
        llm.set_stage("web.synthesize")
        async for ev in llm.stream_chat(
            [{"role": "system", "content": system}, {"role": "user", "content": user}],
            {"max_tokens": _SYNTH_MAX_TOKENS, "temperature": 0},
        ):
            if ev.get("type") == "token":
                delta = ev.get("delta") or ""
                acc.append(delta)
                await progress.emit_token(delta)
    except Exception as e:  # noqa: BLE001 — degrade to the evidence digest
        _log.warning("deep synthesis failed (%s); returning raw evidence", e)
        return evidence
    return "".join(acc)
