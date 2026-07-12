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

"""Own recursive character splitter — cuts on natural
boundaries (paragraph → line → sentence → word), packs toward `chunk_size`, then
prefixes `chunk_overlap` chars of the previous chunk. `chunk_pages` adds the
page-number + clause/section overlay used to populate citation metadata."""

import re

from .config import settings

# clause_section_ref extraction. A statute section / contract
# clause HEADING at the start of a line: a number (`239`, `443A`, `2.3`) with an
# optional `s`/`section` keyword, FOLLOWED BY a title-cased heading word. Requiring a
# capitalised title after the number is what separates a real heading ("239 Ratification
# of acts of directors") from a page number in a running header ("204 Companies Act
# 2006 (c. 46)") or an inline body mention ("239 of the Act provides…").
_CLAUSE_HEAD_RE = re.compile(
    r"^\s*(?:s\.?\s?|section\s+)?(\d+(?:\.\d+)*[A-Za-z]?)[.)]?\s+[\"“(\[A-Z]"
)
# Keyword-gated inline SECTION reference (fallback when no heading leads a line): a stray
# number or amount can't win because the section keyword is required. Deliberately excludes
# `part N` — a running-header Part number is NOT the section a chunk is about (that mislabelled
# continuation chunks with the Part number, e.g. "Part 15" → section 15).
_SECTION_INLINE_RE = re.compile(r"\b(?:s\.?\s?|section\s+)(\d+(?:\.\d+)*[A-Za-z]?)\b", re.IGNORECASE)
# A section HEADING that OWNS operative text in this chunk (follow-up).
# STRICTER than a mention: a section number immediately followed by a Title-case title, with NO
# "s"/"section" keyword (that keyword marks a cross-reference — "Section 561(1) does not apply" —
# not a heading). Two alternatives, both pinned by a `(?=…[A-Z][a-z])` title lookahead:
#   • BARE heading — "564 Exception to pre-emption right: bonus shares" (1-4 digit section).
#   • AMENDMENT-glued heading — the number is fused to a Westlaw annotation marker of VARIABLE width
#     ("[F904566 Exceptions…" F904+566, "[F1102853ADuty…" F1102+853A). A lazy `\[F\d+?` marker plus a
#     2-3 digit section pinned by the title lookahead recovers the REAL section (566A, 853A, 914) —
#     a fixed-width marker guess mis-split these. Both formats slipped past _CLAUSE_HEAD_RE, so the
#     mid-chunk sections s551/s564/s566/s566A/s568 had NO owning chunk and were unfetchable by number.
_OWNED_HEAD_RE = re.compile(
    r"^\s*(?:\[F\d+?(\d{2,3}[A-Za-z]?)|(\d{1,4}(?:\.\d+)*[A-Za-z]?))(?=\s*[A-Z][a-z])"
)
# A commencement/effective date (`1.10.2007`) — must NEVER be mistaken for a section.
_DATE_RE = re.compile(r"\d{1,2}\.\d{1,2}\.\d{2,4}")
# Running-header / metadata boilerplate that carries a leading PAGE number or a Part /
# Chapter label — never the section this chunk is about. Skipped before heading detection.
_BOILERPLATE_RE = re.compile(
    r"companies act|\(c\.\s|document generated|status:|changes to legislation"
    r"|commencement information|royal assent|this version of this act",
    re.IGNORECASE,
)
_STRUCTURE_LEAD_RE = re.compile(r"^\s*(?:part|chapter|schedule)\b", re.IGNORECASE)
# Statute running-header lines: "Part 17 — A company's share capital" / "Chapter 2 —
# Allotment of shares: general provisions". The separator decodes to a mojibake dash in the
# corpus, so we skip any short non-word run before the title (TOC).
# The title cap is generous (statute chapter titles run long, e.g. CA2006 Pt17 Ch3 "Allotment of
# equity securities: existing shareholders' right of pre-emption" = 75 chars — a 70-cap dropped it,
# so the whole pre-emption chapter was missing from the TOC index). Head-only guard bounds abuse.
_PART_LINE_RE = re.compile(r"^\s*Part\s+(\d+[A-Za-z]?)\b[\s\W_]{0,4}([A-Z].{3,120})$", re.MULTILINE)
_CHAP_LINE_RE = re.compile(r"^\s*Chapter\s+(\d+[A-Za-z]?)\b[\s\W_]{0,4}([A-Z].{3,120})$", re.MULTILINE)

_SEPARATORS = ["\n\n", "\n", ". ", " ", ""]


def _split(text: str, size: int, seps: list[str]) -> list[str]:
    if len(text) <= size:
        return [text] if text.strip() else []
    sep = next((s for s in seps if s and s in text), "")
    if sep == "":
        # No separator left: hard-cut.
        return [text[i : i + size] for i in range(0, len(text), size)]
    pieces = text.split(sep)
    rest = seps[seps.index(sep) + 1 :]
    out: list[str] = []
    cur = ""
    for p in pieces:
        cand = f"{cur}{sep}{p}" if cur else p
        if len(cand) <= size:
            cur = cand
        else:
            if cur:
                out.append(cur)
            if len(p) > size:
                out.extend(_split(p, size, rest))
                cur = ""
            else:
                cur = p
    if cur.strip():
        out.append(cur)
    return out


def chunk_text(text: str, size: int | None = None, overlap: int | None = None) -> list[str]:
    from .rag_ctx import cfg

    size = size or cfg("chunk_size", settings.chunk_size)
    overlap = overlap if overlap is not None else cfg("chunk_overlap", settings.chunk_overlap)
    raw = _split(text, size, _SEPARATORS)
    chunks: list[str] = []
    for i, c in enumerate(raw):
        if i > 0 and overlap > 0:
            # Overlap is a raw tail slice of the previous chunk, which would start
            # mid-word ("data" -> "ata"). Snap it to a word boundary so a chunk
            # never starts mid-word — keeps citation labels clean and the leading
            # `quote_text` a verbatim slice of the document. Rejoin with a space.
            tail = raw[i - 1][-overlap:]
            sp = tail.find(" ")
            tail = tail[sp + 1:] if sp != -1 else tail
            c = f"{tail} {c}" if tail.strip() else c
        if c.strip():
            chunks.append(c)
    return chunks


def _is_yearish(ref: str) -> bool:
    """A bare 4-digit year (1900-2099) — amendment/SI-citation lines ("… Regulations
    2009 (S.I. 2009/…)") are riddled with these and they are never a section/clause
    number. Rejected as a ref."""
    return ref.isdigit() and 1900 <= int(ref) <= 2099


def _norm_ref(ref: str) -> str:
    """Normalise a section/clause ref: drop a trailing dot, upper-case a single letter
    suffix (`443a` → `443A`). Dotted contract clauses (`2.3`) pass through unchanged."""
    ref = ref.strip().rstrip(".")
    m = re.fullmatch(r"(\d+(?:\.\d+)*)([A-Za-z])?", ref)
    return m.group(1) + (m.group(2).upper() if m.group(2) else "") if m else ref


def _leading_heading_refs(chunk: str) -> list[str]:
    """Section numbers that HEAD a line in this chunk (statute section headings like
    "549 Exercise by directors of power to allot shares"), first-seen order, with page/
    date/year noise and boilerplate skipped. A chunk can carry several (e.g. "550 …" then
    "551 …") — all are kept so the second section's body is still findable."""
    out: list[str] = []
    for line in (ln.strip() for ln in chunk.splitlines() if ln.strip()):
        if _BOILERPLATE_RE.search(line) or _STRUCTURE_LEAD_RE.match(line):
            continue
        m = _CLAUSE_HEAD_RE.match(line)
        if m and not _DATE_RE.fullmatch(m.group(1)) and not _is_yearish(m.group(1)):
            r = _norm_ref(m.group(1))
            if r not in out:
                out.append(r)
    return out


def _owned_refs(chunk: str) -> list[str]:
    """Section refs whose HEADING (hence operative text) starts a line in this chunk — the
    sections this chunk is an OWNER of, for exact by-number look-up. Stricter than
    `_leading_heading_refs`: bare-number or [F-annotated headings only, never a keyworded in-body
    mention. First-seen order; page/date/year/boilerplate skipped (follow-up)."""
    out: list[str] = []
    for line in (ln.strip() for ln in chunk.splitlines() if ln.strip()):
        if _BOILERPLATE_RE.search(line) or _STRUCTURE_LEAD_RE.match(line):
            continue
        m = _OWNED_HEAD_RE.match(line)
        if not m:
            continue
        num = m.group(1) or m.group(2)  # [F-glued section, else bare section
        if _DATE_RE.fullmatch(num) or _is_yearish(num):
            continue
        r = _norm_ref(num)
        if r not in out:
            out.append(r)
    return out


def _clause_ref(chunk: str) -> str | None:
    """Best-effort clause/section reference for a chunk. A NUMERIC section heading — the
    nearest one heading a line — always wins over
    the ALL-CAPS group heading ("ALLOTMENT OF SHARES: GENERAL PROVISIONS") that used to be
    picked when it appeared on an earlier line, mislabelling the section (s549/s566 class).
    Falls back to an ALL-CAPS heading, then a single unambiguous keyword-gated inline ref."""
    heads = _leading_heading_refs(chunk)
    if heads:
        return heads[0]  # nearest section heading above / first in the chunk
    # A chunk whose ONLY heading is amendment-annotated ("[F904566 Exceptions…") has no _CLAUSE_HEAD
    # match — recover its section from the strict owned-heading pass before the ALL-CAPS fallback,
    # so s566/s566A get a numeric label instead of None (follow-up).
    owned = _owned_refs(chunk)
    if owned:
        return owned[0]
    lines = [ln.strip() for ln in chunk.splitlines() if ln.strip()]
    # Fallback — an ALL-CAPS group heading, only when NO numeric section heading exists.
    for line in lines:
        if _BOILERPLATE_RE.search(line) or _STRUCTURE_LEAD_RE.match(line):
            continue
        letters = [c for c in line if c.isalpha()]
        if len(line) <= 80 and len(letters) >= 4 and line == line.upper():
            return line[:60]
    # Last resort — an explicit inline section reference (keyword-gated), used ONLY when it
    # is unambiguous: a chunk with no leading heading but a SINGLE distinct section number
    # is about/anchored on it. Several distinct inline refs → unattributable → None.
    refs = {_norm_ref(m.group(1)) for m in _SECTION_INLINE_RE.finditer(chunk) if not _is_yearish(m.group(1))}
    return next(iter(refs)) if len(refs) == 1 else None


def _section_refs(chunk: str) -> list[str]:
    """ALL distinct section references a chunk mentions (`refs_out`) —
    the cross-reference graph used at query time to follow "see section 570" to the operative
    text. Union of leading-line section HEADINGS (so a two-section chunk exposes both) and
    keyword-gated inline refs; date/year-filtered. Sorted for a stable payload."""
    refs = set(_leading_heading_refs(chunk))
    refs |= {_norm_ref(m.group(1)) for m in _SECTION_INLINE_RE.finditer(chunk) if not _is_yearish(m.group(1))}
    return sorted(refs)


def _section_num(ref: str | None) -> int | None:
    """Numeric part of a section ref for adjacency (`443A` → 443, `2.3` → 2), suffix and
    sub-levels dropped — so ±N neighbour ranges over `section_num` cluster s443A+s444+s445
. None when there is no leading integer."""
    if not ref:
        return None
    m = re.match(r"(\d+)", ref)
    return int(m.group(1)) if m else None


def toc_header(text: str) -> dict | None:
    """The statute Part/Chapter this chunk sits under, parsed from its running header
. Returns {part, chapter, title} — `title` is the CHAPTER title
    (the topical handle, e.g. "Allotment of shares: general provisions"), falling back to the
    Part title. None when the chunk carries no Part/Chapter header (non-statute KB → TOC inert)."""
    # Only the running header (top of the chunk) counts — a Part/Chapter named in the BODY
    # is a cross-reference, not where the chunk sits, and would pollute the section range.
    head = "\n".join(text.splitlines()[:6])
    pm = _PART_LINE_RE.search(head)
    cm = _CHAP_LINE_RE.search(head)
    if not pm and not cm:
        return None
    part = pm.group(1) if pm else None
    chapter = cm.group(1) if cm else None
    title = (cm.group(2) if cm else pm.group(2)).strip().rstrip(".")
    return {"part": part, "chapter": chapter, "title": title}


def _chunk_meta(text: str) -> dict:
    """The section metadata overlay attached to every chunk: the owning-section ref, the
    full out-reference set, and the numeric section for neighbour ranges."""
    ref = _clause_ref(text)
    meta: dict = {"clause_section_ref": ref, "refs_out": _section_refs(text)}
    num = _section_num(ref)
    if num is not None:
        meta["section_num"] = num
    # follow-up: EVERY section this chunk owns operative text for — not just
    # the primary. A size-split that fell mid-section leaves the next section's heading+body inside
    # the previous chunk (s564 inside the s563 chunk, s568 inside s567); multi-valued `section_nums`
    # lets an exact by-number fetch reach that inner section. Union with the numeric primary so a
    # normally-labelled chunk stays self-findable.
    nums = sorted(
        {n for r in _owned_refs(text) if (n := _section_num(r)) is not None}
        | ({num} if num is not None else set())
    )
    if nums:
        meta["section_nums"] = nums
    return meta


def chunk_pages(
    pages: list[tuple[int, str]], size: int | None = None, overlap: int | None = None
) -> list[dict]:
    """Chunk within page boundaries, attaching `page_number` + a derived
    `clause_section_ref` to each chunk (for citation metadata)."""
    out: list[dict] = []
    for page_no, text in pages:
        for ch in chunk_text(text, size, overlap):
            out.append({"text": ch, "page_number": page_no, **_chunk_meta(ch)})
    return out


def chunk_hierarchy(
    pages: list[tuple[int, str]],
    child_size: int,
    child_overlap: int,
    parent_size: int,
    parent_id_factory,
) -> tuple[list[dict], list[dict]]:
    """L2 parent–child split. Within each page, split into
    parent blocks (the enclosing section, ~`parent_size`), then split each parent
    into smaller children (~`child_size`) for precise retrieval. Returns
    `(parents, children)` where a parent is `{parent_id, text}` and a child is
    `{text, page_number, clause_section_ref, parent_id}`. `parent_id_factory()`
    mints a fresh id per parent (injected so tests stay deterministic)."""
    parents: list[dict] = []
    children: list[dict] = []
    for page_no, text in pages:
        for block in _split(text, parent_size, _SEPARATORS):
            if not block.strip():
                continue
            pid = parent_id_factory()
            parents.append({"parent_id": pid, "text": block})
            for ch in chunk_text(block, child_size, child_overlap):
                children.append({
                    "text": ch,
                    "page_number": page_no,
                    "parent_id": pid,
                    **_chunk_meta(ch),
                })
    return parents, children
