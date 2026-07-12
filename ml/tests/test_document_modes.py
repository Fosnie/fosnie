"""Three whole-document modes: deterministic
token routing (stuff vs map-reduce) and exhaustive map-reduce with structured,
section-tagged accumulation. Fully monkeypatched (no real LLM / extraction)."""

import asyncio

import app.main as main
from app import extract, map_reduce


def test_map_reduce_drops_irrelevant_and_tags_sections(monkeypatch):
    # Three sections; the middle one is irrelevant (NONE) → dropped at reduce.
    monkeypatch.setattr(map_reduce.chunker, "chunk_text", lambda text, size=None: ["AAA", "BBB", "CCC"])

    async def fake_complete(system, user, max_tokens=512):
        if "AAA" in user:
            return "found in A"
        if "CCC" in user:
            return "found in C"
        return "NONE"

    monkeypatch.setattr(map_reduce.llm, "complete", fake_complete)

    out = asyncio.run(map_reduce.map_reduce("ignored", "the task"))
    assert out["mode"] == "map_reduce"
    assert len(out["sections"]) == 2, "the irrelevant section is not accumulated"
    # Structured accumulation keeps the section anchor and original order.
    assert out["sections"][0]["section_ref"] == "section 1"
    assert out["sections"][1]["section_ref"] == "section 3"
    assert "found in A" in out["text"] and "found in C" in out["text"]
    assert "NONE" not in out["text"]


def test_map_reduce_drops_none_with_punctuation(monkeypatch):
    # Models append punctuation/casing — "NONE." must still be dropped.
    monkeypatch.setattr(map_reduce.chunker, "chunk_text", lambda text, size=None: ["A", "B"])

    async def fake_complete(system, user, max_tokens=512):
        return "NONE." if "A" in user.split("Section:")[-1] else "real finding"

    monkeypatch.setattr(map_reduce.llm, "complete", fake_complete)
    out = asyncio.run(map_reduce.map_reduce("ignored", "task"))
    assert len(out["sections"]) == 1
    assert out["sections"][0]["result"] == "real finding"


def _resolve_stub(max_len: int):
    async def f():
        return ("model", max_len)
    return f


def test_read_document_stuffs_small_doc(monkeypatch):
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    monkeypatch.setattr(main, "_resolve_model", _resolve_stub(1000))
    monkeypatch.setattr(extract, "extract", lambda path, mime: "tiny document")

    req = main.ReadDocumentRequest(path="x", prompt="find stuff")
    out = asyncio.run(main.read_document(req))
    assert out["mode"] == "stuff"
    assert out["text"] == "tiny document"
    assert out["truncated"] is False


def test_read_document_map_reduces_large_doc_with_prompt(monkeypatch):
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    monkeypatch.setattr(main, "_resolve_model", _resolve_stub(1000))  # stuff budget = 450 tokens
    big = "word " * 5000  # ~25k chars ≈ 6.25k tokens ≫ 450 → must map-reduce
    monkeypatch.setattr(extract, "extract", lambda path, mime: big)

    async def fake_mr(text, prompt):
        return {"mode": "map_reduce", "sections": [{"section_ref": "section 1", "result": "r"}], "text": "[section 1]\nr"}

    monkeypatch.setattr(map_reduce, "map_reduce", fake_mr)

    req = main.ReadDocumentRequest(path="x", prompt="find clause")
    out = asyncio.run(main.read_document(req))
    assert out["mode"] == "map_reduce"
    assert out["sections"] == 1
    assert "section 1" in out["text"]


def test_read_document_large_no_prompt_stuffs_truncated(monkeypatch):
    # No task to map against → stuff, but capped to the budget (never feed an
    # over-long doc silently).
    monkeypatch.setattr(main, "safe_path", lambda p: p)
    monkeypatch.setattr(main, "_resolve_model", _resolve_stub(1000))
    big = "x" * 10000  # 10k chars ≫ stuff budget (450 tokens ≈ 1800 chars)
    monkeypatch.setattr(extract, "extract", lambda path, mime: big)

    req = main.ReadDocumentRequest(path="x")  # no prompt
    out = asyncio.run(main.read_document(req))
    assert out["mode"] == "stuff"
    assert out["truncated"] is True
    assert len(out["text"]) <= 1800
