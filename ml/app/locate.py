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

"""Locate a decomposed claim back to its verbatim span in the source document.
Decomposition rephrases claims
(resolves pronouns, rewrites), so a claim is not a substring of the document;
this finds the best-matching contiguous span so the inline highlighter can wrap
it and ground-or-cut repair can use it as the tracked-change `find` text.

Pure stdlib (`difflib`). Returns `None` below a confidence floor — an unlocatable
claim is simply not highlighted and not repairable (honest degradation)."""

import difflib
import re

_WORD = re.compile(r"\w+")
# Crude sentence chunks: up to a terminator, or a trailing run with none.
_SENT = re.compile(r"[^.!?\n]*[.!?]+|[^.!?\n]+")

# Coarse-anchor padding and the whole-document size under which we skip the
# anchor and diff the full text (cheap enough; avoids a mis-anchored window).
_PAD = 600
_FULL_SCAN_LIMIT = 20_000


def _anchor_window(claim: str, text: str, hint_start: int) -> tuple[int, str]:
    """A bounded slice of `text` likely to contain the claim, so the diff stays
    cheap on long documents. Anchored on a distinctive claim word near
    `hint_start` (the claim's section offset), falling back to its first global
    occurrence, then to a window at the hint / the whole text."""
    if len(text) <= _FULL_SCAN_LIMIT:
        return 0, text
    words = sorted({w for w in _WORD.findall(claim) if len(w) >= 5}, key=len, reverse=True)
    anchor = -1
    for w in words[:6]:
        idx = text.find(w, max(0, hint_start - _PAD))
        if idx == -1:
            idx = text.find(w)
        if idx != -1:
            anchor = idx
            break
    if anchor == -1:
        s = max(0, hint_start - _PAD)
        return s, text[s : s + 4_000]
    span = max(len(claim) * 3, 800)
    s = max(0, anchor - _PAD)
    e = min(len(text), anchor + span + _PAD)
    return s, text[s:e]


def _snap(text: str, start: int, end: int) -> tuple[int, int]:
    """Grow to whole words, then trim surrounding whitespace."""
    while start > 0 and (text[start - 1].isalnum() or text[start - 1] == "'"):
        start -= 1
    while end < len(text) and (text[end].isalnum() or text[end] == "'"):
        end += 1
    while start < end and text[start].isspace():
        start += 1
    while end > start and text[end - 1].isspace():
        end -= 1
    return start, end


def _cover(sm: difflib.SequenceMatcher, sent: str, claim_len: int) -> float:
    """Fraction of the claim accounted for by its longest common subsequence with
    `sent` — high when the sentence carries most of the claim's words."""
    sm.set_seq1(sent.lower())
    matched = sum(b.size for b in sm.get_matching_blocks())
    return matched / max(1, claim_len)


def locate(claim: str, text: str, hint_start: int = 0, min_cover: float = 0.5) -> dict | None:
    """Locate the claim to the single best-matching **sentence** of `text`, or
    `None`. Returns `{start, end, text}` (absolute char offsets + the verbatim
    sentence). A sentence is a naturally-bounded unit — clean to highlight and to
    rewrite/cut as a tracked change — and avoids the over-reach of stitching
    scattered word matches across sentences. `min_cover` is the share of the claim
    the sentence must carry to be trusted; a synthesised claim binds to the
    sentence holding its distinctive (predicate-bearing) words."""
    if not claim or not claim.strip() or not text:
        return None
    w_start, window = _anchor_window(claim, text, hint_start)
    sm = difflib.SequenceMatcher(autojunk=False)
    sm.set_seq2(claim.lower())
    claim_len = len(claim)
    best: tuple[float, int, int] | None = None  # (cover, start, end)
    for m in _SENT.finditer(window):
        sent = m.group()
        if not sent.strip():
            continue
        cover = _cover(sm, sent, claim_len)
        if best is None or cover > best[0]:
            best = (cover, m.start(), m.end())
    if best is None or best[0] < min_cover:
        return None
    start, end = _snap(text, w_start + best[1], w_start + best[2])
    if end <= start:
        return None
    return {"start": start, "end": end, "text": text[start:end]}


def section_offsets(text: str, sections: list[str]) -> list[int]:
    """Best-effort start offset of each section in `text`. Sections may carry a
    chunk-overlap prefix, so this is a coarse hint for `locate`, not exact."""
    offsets: list[int] = []
    cursor = 0
    for s in sections:
        probe = s.strip()[:80]
        pos = text.find(probe, cursor) if probe else -1
        if pos == -1 and probe:
            pos = text.find(probe)
        off = pos if pos != -1 else cursor
        offsets.append(off)
        cursor = max(cursor, off + 1)
    return offsets
