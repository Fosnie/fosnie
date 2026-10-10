# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Deterministic, source-preserving excerpts for the deliberately partial quick path.

This is a lexical prefilter, not a model-quality classifier. It never fetches,
calls inference, changes a source or treats question/source text as instructions.
"""
import math
import re
from collections import Counter

MAX_CANDIDATES = 8
MAX_EXCERPT_CHARS = 800
MAX_DIGEST_CHUNKS = 4
MAX_QUESTION_CHARS = 2048
_WORD = re.compile(r"[^\W_]+", re.UNICODE)


def ranking_query(query: str, question: str | None) -> str:
    # The question is supplied separately by Core, never by tool arguments.
    query = query[:512].strip()
    focus = (question or "")[:1536].strip()
    return f"{query}\n{focus}" if focus else query


def _terms(text: str) -> set[str]:
    return {m.group().casefold() for m in _WORD.finditer(text) if len(m.group()) > 1}


def _weights(query: str, pairs: list[tuple[int, str]]) -> dict[str, float]:
    terms = _terms(query)
    counts = Counter(term for _, text in pairs for term in _terms(text) & terms)
    return {term: math.log(1 + (len(pairs) + 1) / (counts[term] + 1)) for term in terms}


def _score(text: str, weights: dict[str, float]) -> float:
    # Unique terms: repeating a keyword cannot dominate the shortlist.
    return sum(weights.get(term, 0.0) for term in sorted(_terms(text)))


def _excerpt(text: str, weights: dict[str, float]) -> str:
    if len(text) <= MAX_EXCERPT_CHARS:
        return text
    starts = {0}
    for match in _WORD.finditer(text):
        if match.group().casefold() in weights:
            starts.add(min(max(0, match.start() - MAX_EXCERPT_CHARS // 4),
                           len(text) - MAX_EXCERPT_CHARS))
    # A contiguous verbatim window keeps negation, values and nearby caveats;
    # ties favour the earlier window. No ellipses become invented source text.
    start = max(sorted(starts), key=lambda n: _score(text[n:n + MAX_EXCERPT_CHARS], weights))
    return text[start:start + MAX_EXCERPT_CHARS]


def _diverse_indices(pairs: list[tuple[int, str]], scores: list[float], cap: int) -> list[int]:
    order = sorted(range(len(pairs)), key=lambda i: scores[i], reverse=True)
    chosen = []
    sources = set()
    for i in order:
        if pairs[i][0] not in sources:
            chosen.append(i)
            sources.add(pairs[i][0])
            if len(chosen) == cap:
                return chosen
    selected = set(chosen)
    return (chosen + [i for i in order if i not in selected])[:cap]


def shortlist(query: str, pairs: list[tuple[int, str]]) -> tuple[list[tuple[int, str]], bool]:
    weights = _weights(query, pairs)
    excerpts = [(source, _excerpt(text, weights)) for source, text in pairs]
    indices = _diverse_indices(excerpts, [_score(text, weights) for _, text in excerpts], MAX_CANDIDATES)
    selected = [excerpts[i] for i in indices]
    partial = len(selected) < len(pairs) or any(len(text) > MAX_EXCERPT_CHARS for _, text in pairs)
    return selected, partial


def pick(pairs: list[tuple[int, str]], scores: list[float]) -> list[tuple[int, str]]:
    if len(scores) != len(pairs) or any(not math.isfinite(score) for score in scores):
        raise ValueError("quick web rerank scores must be complete and finite")
    return [pairs[i] for i in _diverse_indices(pairs, scores, MAX_DIGEST_CHUNKS)]
