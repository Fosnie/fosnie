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

"""Claim decomposition for Mode B "Verify draft".
Decomposition quality dominates
verification quality, so this follows the **Claimify/SAFE pattern**, not a naive
"split into claims": sentence-split → select verifiable content → resolve
referents from local context → **flag-and-skip a sentence whose referents cannot
be confidently resolved** (never guess — a decontextualised claim is the main
source of false flags) → emit standalone atomic claims. The main LLM does this
once per section; it is the single biggest quality lever (spec)."""

import json
import logging

from . import llm

_log = logging.getLogger("pai.decompose")

_SYSTEM = (
    "You extract atomic, verifiable factual claims from a passage so each can be "
    "fact-checked against source documents. Follow these rules exactly:\n"
    "1. Split the passage into individual sentences.\n"
    "2. Keep ONLY verifiable factual statements. Drop opinions, recommendations, "
    "hedges, questions, headings, and meta-text (e.g. 'this document explains').\n"
    "3. Make every claim STANDALONE: resolve pronouns and references ('it', 'they', "
    "'this', 'the company', 'the agreement') using the passage so the claim is "
    "understandable on its own, with no outside context.\n"
    "4. If a sentence's referent CANNOT be confidently resolved from the passage, "
    "SKIP that sentence — never guess.\n"
    "5. Break compound sentences into atomic, one-fact claims.\n"
    'Return ONLY a JSON array of claim strings, e.g. ["...", "..."]. No prose.'
)


def _parse(raw: str) -> list[str]:
    """Pull a JSON array of claim strings from the model output (tolerant of
    markdown/prose wrappers); drop anything that isn't a non-empty string."""
    s = raw.strip()
    start, end = s.find("["), s.rfind("]")
    if start == -1 or end <= start:
        return []
    try:
        arr = json.loads(s[start : end + 1])
    except (ValueError, TypeError):
        return []
    out: list[str] = []
    for c in arr:
        if isinstance(c, str) and c.strip():
            out.append(c.strip())
    # de-duplicate, preserve order
    return list(dict.fromkeys(out))


async def decompose_claims(text: str) -> list[str]:
    """Decompose one passage/section into standalone atomic claims. Returns [] on
    an empty/unverifiable passage or a model/parse failure (the job continues)."""
    if not text or not text.strip():
        return []
    try:
        raw = await llm.complete(_SYSTEM, text, max_tokens=1024)
    except Exception as e:  # noqa: BLE001 — a bad section must not kill the job
        _log.warning("decompose failed for a section: %s", e)
        return []
    return _parse(raw)
