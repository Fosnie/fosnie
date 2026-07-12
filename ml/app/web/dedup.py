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

"""Syndication dedup: wire-service copy is
republished near-verbatim across many outlets; without dedup the digest fills
with the same paragraphs from five domains. Cheap deterministic shingle-Jaccard
over the lead text — no model, unit-testable, fast."""

import re

_LEAD_CHARS = 1200
_K = 5  # words per shingle

_norm_re = re.compile(r"[^a-z0-9\s]+")


def shingles(text: str, k: int = _K) -> set[str]:
    """k-word shingles of the normalised lead text."""
    lead = (text or "")[:_LEAD_CHARS].lower()
    words = _norm_re.sub(" ", lead).split()
    if len(words) < k:
        return {" ".join(words)} if words else set()
    return {" ".join(words[i : i + k]) for i in range(len(words) - k + 1)}


def jaccard(a: set[str], b: set[str]) -> float:
    if not a or not b:
        return 0.0
    inter = len(a & b)
    if inter == 0:
        return 0.0
    return inter / len(a | b)


def is_near_duplicate(text: str, pool: list[set[str]], threshold: float) -> bool:
    """True when `text`'s lead shingles overlap any pool entry above `threshold`."""
    s = shingles(text)
    if not s:
        return False
    return any(jaccard(s, other) >= threshold for other in pool)
