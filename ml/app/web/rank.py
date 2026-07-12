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

"""Composite URL ranking for the agentic loop:
cross-variant frequency + domain-tier trust prior + path-depth decay + reranker
score of (sub-question vs title+snippet). The tier is a rank PRIOR, never a hard
filter — agents otherwise prefer SEO farms over authoritative sources, but an
authoritative answer on a low-tier domain must still be reachable."""

from urllib.parse import urlsplit

# Trust prior by registrable-domain suffix (longest suffix wins). Deliberately a
# code constant, not config — it encodes editorial judgement, not deployment shape.
_DOMAIN_TIERS: dict[str, float] = {
    # Government / standards / primary authorities.
    "gov": 1.0,
    "edu": 1.0,
    "gov.uk": 1.0,
    "ac.uk": 1.0,
    "europa.eu": 1.0,
    "who.int": 1.0,
    "w3.org": 1.0,
    "ietf.org": 1.0,
    "iso.org": 1.0,
    "nist.gov": 1.0,
    "legislation.gov.uk": 1.0,
    # Primary documentation / reference / technical sources.
    "wikipedia.org": 0.85,
    "github.com": 0.85,
    "arxiv.org": 0.85,
    "stackoverflow.com": 0.85,
    "docs.python.org": 0.85,
    "developer.mozilla.org": 0.85,
    "rust-lang.org": 0.85,
    # Major press.
    "reuters.com": 0.75,
    "apnews.com": 0.75,
    "bbc.co.uk": 0.75,
    "bbc.com": 0.75,
    "ft.com": 0.75,
    "theguardian.com": 0.75,
    "bloomberg.com": 0.75,
    "nytimes.com": 0.75,
    # Engagement-farmed / low-signal platforms (demoted, not excluded).
    "pinterest.com": 0.25,
    "pinterest.co.uk": 0.25,
    "quora.com": 0.25,
    "medium.com": 0.25,
}

_DEFAULT_TIER = 0.5


def tier(domain: str) -> float:
    """Trust prior for a host, by longest matching suffix; 0.5 for unknown."""
    host = (domain or "").lower().lstrip(".")
    best_len = -1
    best = _DEFAULT_TIER
    for suffix, score in _DOMAIN_TIERS.items():
        if (host == suffix or host.endswith("." + suffix)) and len(suffix) > best_len:
            best_len = len(suffix)
            best = score
    return best


def path_depth(url: str) -> int:
    """Non-empty path segments — deeper paths decay (listing/article pages near
    the root tend to be the canonical copies)."""
    return len([s for s in urlsplit(url).path.split("/") if s])


def normalise(scores: list[float]) -> list[float]:
    """Min-max normalise reranker scores across the candidate set. A degraded
    reranker (all-equal scores) yields a flat 0.5 so the other signals still
    order the candidates."""
    if not scores:
        return []
    lo, hi = min(scores), max(scores)
    if hi - lo < 1e-9:
        return [0.5] * len(scores)
    return [(s - lo) / (hi - lo) for s in scores]


def composite(rerank_norm: float, freq: int, domain: str, url: str) -> float:
    """Final candidate score. `freq` = number of distinct (sub-question, variant)
    SERPs the URL appeared in (cross-source agreement, capped at 3)."""
    return (
        0.5 * rerank_norm
        + 0.2 * (min(freq, 3) / 3.0)
        + 0.2 * tier(domain)
        + 0.1 * (1.0 / (1.0 + path_depth(url)))
    )
