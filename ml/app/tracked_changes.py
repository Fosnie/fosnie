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

"""DOCX tracked changes — ported from the TypeScript implementation (docxTrackedChanges.ts) to
Python/lxml. Correctness-critical: an invalid
DOCX is a lawyer who cannot open their document. lxml is isolated behind this
module (same pattern as the auth-crate wrapper).

Operates on file paths (in/out) to match the on-disk contract used by /ingest
and /read-document — no base64 of large files over HTTP.

The paragraph flattener here is the SINGLE shared flattener: `extract.py` uses
it for `read_document` so the assistant's view of a DOCX matches what the
tracked-change writer sees (shared-flattener invariant). Pre-existing `<w:ins>` are shown
as accepted (text inline); pre-existing `<w:del>` are hidden (accepted view).

Scope: body paragraphs only — headers/footers/footnotes/comments
are not handled (SYNTHESIS F.1). Re-zipping normalises any older-Windows
backslash entry paths (`word\\document.xml`) to forward slashes.
"""

from __future__ import annotations

import copy
import zipfile
from dataclasses import dataclass
from datetime import datetime, timezone

from lxml import etree

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"
XML_SPACE = "{http://www.w3.org/XML/1998/namespace}space"


def _w(tag: str) -> str:
    return f"{{{W}}}{tag}"


# --- DOCX zip read/write (backslash-path workaround) -------------------------


def _load_docx(path: str) -> tuple[dict[str, bytes], str]:
    """Read every zip entry into memory; return (entries, document.xml key).
    Tolerates `word\\document.xml` backslash entries (older Windows Word)."""
    with zipfile.ZipFile(path) as z:
        entries = {name: z.read(name) for name in z.namelist()}
    doc_key = next(
        (n for n in entries if n.replace("\\", "/") == "word/document.xml"), None
    )
    if doc_key is None:
        raise ValueError("not a Word document: word/document.xml missing")
    return entries, doc_key


def _save_docx(out_path: str, entries: dict[str, bytes], doc_key: str, doc_xml: bytes) -> None:
    """Re-zip, replacing document.xml and normalising backslash entry paths to
    forward slashes (what Word/LibreOffice expect)."""
    with zipfile.ZipFile(out_path, "w", zipfile.ZIP_DEFLATED) as z:
        for name, data in entries.items():
            payload = doc_xml if name == doc_key else data
            z.writestr(name.replace("\\", "/"), payload)


def _parse(doc_xml: bytes) -> etree._Element:
    # SECURITY: a DOCX is attacker-controllable input. `etree.fromstring`'s default
    # parser RESOLVES external entities, so a crafted `word/document.xml` carrying
    # `<!DOCTYPE x [<!ENTITY e SYSTEM "file:///…">]>` + `&e;` would read arbitrary
    # server files (config secrets, other tenants' documents) and inline them into
    # the flattened text — a classic XXE disclosure. Disable entity resolution, DTD
    # loading and network access. Valid WordprocessingML never declares custom
    # entities (predefined `&amp;`/`&lt;`/numeric refs are unaffected), so hardening
    # does not change parsing of legitimate documents.
    # A fresh parser per call: lxml parsers are not safe to share across threads,
    # and tracked-change ops may run in a worker threadpool.
    # huge_tree off + recover off: fail loudly on malformed XML and cap entity bombs.
    parser = etree.XMLParser(
        resolve_entities=False,
        no_network=True,
        load_dtd=False,
        dtd_validation=False,
        huge_tree=False,
        recover=False,
    )
    return etree.fromstring(doc_xml, parser=parser)


def _serialise(root: etree._Element) -> bytes:
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


# --- Paragraph flattening (the shared invariant) -----------------------------


@dataclass
class _Char:
    ch: str
    rpr: etree._Element | None  # the originating run's <w:rPr> (shared identity)


def _flatten_paragraph(p: etree._Element) -> list[_Char]:
    """Accepted-view char list for one <w:p>: <w:r> text inline, <w:ins> inlined,
    <w:del> hidden. Each char carries its run's rPr (by identity, for regrouping)."""
    chars: list[_Char] = []

    def emit_run(r: etree._Element) -> None:
        rpr = r.find(_w("rPr"))
        for node in r:
            tag = etree.QName(node).localname
            if tag in ("t", "delText"):
                for c in node.text or "":
                    chars.append(_Char(c, rpr))
            elif tag in ("br", "cr"):
                chars.append(_Char("\n", rpr))
            elif tag == "tab":
                chars.append(_Char("\t", rpr))

    for child in p:
        tag = etree.QName(child).localname
        if tag == "r":
            emit_run(child)
        elif tag == "ins":  # accepted view: inline its runs
            for r in child.findall(_w("r")):
                emit_run(r)
        # del: hidden in accepted view; pPr/bookmarks/etc: not text
    return chars


def _paragraph_text(p: etree._Element) -> str:
    return "".join(c.ch for c in _flatten_paragraph(p))


def extract_body_text(path: str) -> str:
    """Plain-text flatten of the body (accepted view) — the `read_document`
    surface. Same flattener the writer uses."""
    entries, doc_key = _load_docx(path)
    root = _parse(entries[doc_key])
    body = root.find(_w("body"))
    if body is None:
        return ""
    return "\n".join(_paragraph_text(p) for p in body.iter(_w("p")))


# --- Element construction ----------------------------------------------------


def _make_run(text: str, rpr: etree._Element | None, del_text: bool = False) -> etree._Element:
    r = etree.Element(_w("r"))
    if rpr is not None:
        r.append(copy.deepcopy(rpr))
    t = etree.SubElement(r, _w("delText") if del_text else _w("t"))
    t.set(XML_SPACE, "preserve")
    t.text = text
    return r


def _make_marker(kind: str, w_id: int, author: str, date: str) -> etree._Element:
    el = etree.Element(_w(kind))  # 'ins' or 'del'
    el.set(_w("id"), str(w_id))
    el.set(_w("author"), author)
    el.set(_w("date"), date)
    return el


# --- Matching ----------------------------------------------------------------


def _find_span(
    text: str, find: str, ctx_before: str | None, ctx_after: str | None
) -> int | None:
    """First index of `find` in `text` honouring surrounding context. For an
    empty `find` (pure insertion), anchors after `ctx_before`."""
    if find == "":
        if ctx_before:
            i = text.find(ctx_before)
            return i + len(ctx_before) if i >= 0 else None
        return 0
    start = 0
    while True:
        i = text.find(find, start)
        if i < 0:
            return None
        if ctx_before and not text[:i].endswith(ctx_before):
            start = i + 1
            continue
        if ctx_after and not text[i + len(find):].startswith(ctx_after):
            start = i + 1
            continue
        return i


# --- Apply -------------------------------------------------------------------


@dataclass
class _PlannedEdit:
    w_id: int
    find: str
    replace: str
    start: int  # char offset in the paragraph's flattened text
    end: int


def _next_w_id(root: etree._Element) -> int:
    """One above the max existing w:id on any ins/del, so new ids never collide."""
    mx = 0
    for el in root.iter(_w("ins"), _w("del")):
        v = el.get(_w("id"))
        if v and v.lstrip("-").isdigit():
            mx = max(mx, int(v))
    return mx + 1


def _rebuild_paragraph(
    p: etree._Element, chars: list[_Char], edits: list[_PlannedEdit], author: str, date: str
) -> None:
    """Replace the run-level content of `p` with keep/del/ins runs. `pPr` and any
    leading non-run elements are preserved; other inline children of a matched
    paragraph are not (acceptable for body prose — see module docstring)."""
    edits = sorted(edits, key=lambda e: e.start)

    # Preserve leading <w:pPr> (must stay first); drop existing run-level content.
    ppr = p.find(_w("pPr"))
    for child in list(p):
        p.remove(child)
    if ppr is not None:
        p.append(ppr)

    def append_keep(seg: list[_Char]) -> None:
        # Group consecutive chars by their rPr identity to preserve formatting.
        i = 0
        while i < len(seg):
            j = i + 1
            while j < len(seg) and seg[j].rpr is seg[i].rpr:
                j += 1
            text = "".join(c.ch for c in seg[i:j])
            p.append(_make_run(text, seg[i].rpr))
            i = j

    cursor = 0
    for e in edits:
        if e.start < cursor:
            continue  # overlapping edit; skip (already reported as error)
        append_keep(chars[cursor:e.start])
        rpr = chars[e.start].rpr if e.start < len(chars) else None
        if e.find:
            d = _make_marker("del", e.w_id, author, date)
            d.append(_make_run(e.find, rpr, del_text=True))
            p.append(d)
        if e.replace:
            ins = _make_marker("ins", e.w_id, author, date)
            ins.append(_make_run(e.replace, rpr))
            p.append(ins)
        cursor = e.end
    append_keep(chars[cursor:])


def apply_tracked_changes(
    in_path: str, out_path: str, edits: list[dict], author: str = "Assistant"
) -> dict:
    """Apply find/replace `edits` as tracked changes. Returns
    {changes:[{w_id,find,replace}], errors:[{index,reason}]}. One stable `w_id`
    per logical edit (shared by its <w:del> and <w:ins>)."""
    entries, doc_key = _load_docx(in_path)
    root = _parse(entries[doc_key])
    body = root.find(_w("body"))
    if body is None:
        raise ValueError("document has no body")

    paragraphs = list(body.iter(_w("p")))
    flat = {id(p): _flatten_paragraph(p) for p in paragraphs}
    text = {id(p): "".join(c.ch for c in flat[id(p)]) for p in paragraphs}

    next_id = _next_w_id(root)
    per_para: dict[int, list[_PlannedEdit]] = {}
    changes: list[dict] = []
    errors: list[dict] = []

    for idx, edit in enumerate(edits):
        find = edit.get("find", "") or ""
        replace = edit.get("replace", "") or ""
        cb = edit.get("context_before")
        ca = edit.get("context_after")
        if find == "" and replace == "":
            errors.append({"index": idx, "reason": "empty edit"})
            continue

        located: tuple[etree._Element, int] | None = None
        for p in paragraphs:
            pos = _find_span(text[id(p)], find, cb, ca)
            if pos is not None:
                located = (p, pos)
                break
        if located is None:
            errors.append({"index": idx, "reason": "find text not located in context"})
            continue

        p, pos = located
        span = (pos, pos + len(find))
        # Reject overlap with an already-planned edit in the same paragraph.
        if any(not (span[1] <= e.start or span[0] >= e.end) for e in per_para.get(id(p), [])):
            errors.append({"index": idx, "reason": "overlaps another edit"})
            continue

        w_id = next_id
        next_id += 1
        per_para.setdefault(id(p), []).append(
            _PlannedEdit(w_id, find, replace, span[0], span[1])
        )
        changes.append({"w_id": str(w_id), "find": find, "replace": replace})

    date = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    for p in paragraphs:
        planned = per_para.get(id(p))
        if planned:
            _rebuild_paragraph(p, flat[id(p)], planned, author, date)

    _save_docx(out_path, entries, doc_key, _serialise(root))
    return {"changes": changes, "errors": errors}


# --- Resolve -----------------------------------------------------------------


def _unwrap(el: etree._Element) -> None:
    """Replace `el` with its children, in place."""
    parent = el.getparent()
    idx = parent.index(el)
    for child in list(el):
        el.remove(child)
        parent.insert(idx, child)
        idx += 1
    parent.remove(el)


def _deltext_to_text(el: etree._Element) -> None:
    for dt in el.iter(_w("delText")):
        dt.tag = _w("t")


def _resolve_elements(root: etree._Element, targets: list[etree._Element], action: str) -> None:
    for el in targets:
        tag = etree.QName(el).localname
        if action == "accept":
            if tag == "ins":
                _unwrap(el)          # keep inserted text
            else:  # del
                el.getparent().remove(el)  # drop deleted text
        elif action == "reject":
            if tag == "ins":
                el.getparent().remove(el)  # drop inserted text
            else:  # del
                _deltext_to_text(el)
                _unwrap(el)          # restore deleted text
        else:
            raise ValueError(f"unknown action: {action}")


def resolve_tracked_change(in_path: str, out_path: str, w_id: str, action: str) -> dict:
    """Accept/reject one change (its <w:del> and <w:ins> share `w_id`)."""
    entries, doc_key = _load_docx(in_path)
    root = _parse(entries[doc_key])
    targets = [
        el for el in root.iter(_w("ins"), _w("del")) if el.get(_w("id")) == str(w_id)
    ]
    if not targets:
        raise ValueError(f"no tracked change with id {w_id}")
    _resolve_elements(root, targets, action)
    _save_docx(out_path, entries, doc_key, _serialise(root))
    return {"resolved": [str(w_id)]}


def resolve_all(
    in_path: str, out_path: str, action: str, author_filter: str | None = None
) -> dict:
    """Accept/reject all changes, optionally only those by `author_filter`."""
    entries, doc_key = _load_docx(in_path)
    root = _parse(entries[doc_key])
    targets = [
        el
        for el in root.iter(_w("ins"), _w("del"))
        if author_filter is None or el.get(_w("author")) == author_filter
    ]
    resolved = sorted({el.get(_w("id")) for el in targets if el.get(_w("id"))})
    _resolve_elements(root, targets, action)
    _save_docx(out_path, entries, doc_key, _serialise(root))
    return {"resolved": resolved}
