"""Chunker unit checks. Run: `uv run python tests/test_chunker.py` from ml/."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.chunker import _clause_ref, chunk_hierarchy, chunk_pages, chunk_text  # noqa: E402


def test_small_text_is_one_chunk():
    assert chunk_text("hello world", size=100, overlap=10) == ["hello world"]


def test_splits_and_respects_size_with_overlap():
    text = "Para one is here.\n\n" + ("alpha beta gamma delta " * 200)
    chunks = chunk_text(text, size=200, overlap=50)
    assert len(chunks) > 1
    # Each chunk is at most size + the overlap prefix (+ a small slack).
    assert all(len(c) <= 200 + 50 + 5 for c in chunks)


def test_overlap_prefixes_previous_tail():
    text = "abcdefghij" * 50  # 500 chars, no separators
    chunks = chunk_text(text, size=100, overlap=20)
    assert len(chunks) >= 2
    # chunk[1] begins with the last 20 chars of the raw second slice's predecessor
    assert chunks[1][:20] == chunks[0][-20:] or len(chunks[0]) <= 20


def test_overlap_is_word_aligned_no_midword_start():
    # With prose, the overlap prefix must snap to a word boundary so no chunk
    # begins mid-word (keeps citation labels clean + quotes verbatim).
    text = ("the quick brown fox jumps over the lazy dog " * 40).strip()
    chunks = chunk_text(text, size=120, overlap=30)
    assert len(chunks) > 2
    words = set(text.split())
    for c in chunks:
        first = c.split()[0]
        assert first in words, f"chunk starts mid-word: {first!r}"


def test_clause_ref_numbered_and_caps():
    assert _clause_ref("2.3 The term may be extended by notice.") == "2.3"
    assert _clause_ref("1. TERM\nThe agreement runs for 12 months.") == "1"
    assert _clause_ref("LIMITATION OF LIABILITY\nNeither party shall...") == "LIMITATION OF LIABILITY"
    assert _clause_ref("just some prose without a heading") is None


def test_chunk_pages_carries_page_and_clause():
    pages = [
        (1, "1. TERM\nThe term is twelve months."),
        (2, "Ordinary prose on the second page about delivery."),
    ]
    items = chunk_pages(pages, size=200, overlap=0)
    assert items, "produces chunks"
    p1 = [it for it in items if it["page_number"] == 1]
    p2 = [it for it in items if it["page_number"] == 2]
    assert p1 and p1[0]["clause_section_ref"] == "1"
    assert p2 and p2[0]["page_number"] == 2


def test_chunk_hierarchy_parent_child():
    page_text = "1. TERM\n" + ("alpha " * 120) + "\n\n2. FEES\n" + ("beta " * 120)
    pages = [(1, page_text)]
    counter = {"n": 0}

    def factory():
        counter["n"] += 1
        return f"p{counter['n']}"

    parents, children = chunk_hierarchy(
        pages, child_size=150, child_overlap=0, parent_size=500, parent_id_factory=factory
    )
    assert parents and children
    assert len(children) > len(parents), "children are finer-grained than parents"

    pmap = {p["parent_id"]: p["text"] for p in parents}
    for c in children:
        assert c["parent_id"] in pmap, "every child maps to a real parent"
        assert c["text"] in pmap[c["parent_id"]], "child ⊆ parent (overlap=0)"
        assert len(c["text"]) <= 150 + 5
        assert c["page_number"] == 1
    for p in parents:
        assert p["text"] in page_text, "parent ⊆ page"
    # The clause overlay still fires on a child.
    assert any(c["clause_section_ref"] == "1" for c in children)


if __name__ == "__main__":
    test_small_text_is_one_chunk()
    test_splits_and_respects_size_with_overlap()
    test_overlap_prefixes_previous_tail()
    test_clause_ref_numbered_and_caps()
    test_chunk_pages_carries_page_and_clause()
    test_chunk_hierarchy_parent_child()
    print("chunker tests ok")
