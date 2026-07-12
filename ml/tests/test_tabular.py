"""Tabular cell-generation tests. No network: `llm.complete` is monkeypatched so
the bounded-concurrency pool, JSON coaxing and value typing are exercised without
a real LLM."""

import asyncio
import zipfile

import pytest

from app import llm, tabular


def test_format_suffix_and_coercion():
    assert "Yes or No" in tabular.format_suffix("yes_no")
    assert tabular.coerce_value("yes_no", "Yes") is True
    assert tabular.coerce_value("yes_no", "no") is False
    assert tabular.coerce_value("number", "about 42 items") == 42.0
    assert tabular.coerce_value("percentage", "12.5%") == 12.5
    assert tabular.coerce_value("bulleted_list", "- a\n- b\n- c") == ["a", "b", "c"]
    assert tabular.coerce_value("text", "prose answer") == "prose answer"


def test_parse_json_tolerates_prose_and_fences():
    assert tabular._parse_json('{"value": "x"}')["value"] == "x"
    assert tabular._parse_json('here you go: {"value": 1, "reasoning": "r"}')["value"] == 1
    # No JSON at all → whole text becomes the value.
    assert tabular._parse_json("just text")["value"] == "just text"


def test_generate_review_streams_all_cells(monkeypatch, tmp_path):
    # Two tiny docs × two columns = four cells.
    d1 = tmp_path / "a.txt"
    d1.write_text("The term is 12 months. Liability is capped at 1,000,000 GBP.")
    d2 = tmp_path / "b.txt"
    d2.write_text("The term is 24 months. Liability is uncapped.")
    documents = [
        {"document_id": "doc1", "path": str(d1), "mime": "text/plain"},
        {"document_id": "doc2", "path": str(d2), "mime": "text/plain"},
    ]
    columns = [
        {"key": "term", "format": "text", "prompt": "What is the term?"},
        {"key": "capped", "format": "yes_no", "prompt": "Is liability capped?"},
    ]

    seen_concurrency = {"max": 0, "cur": 0}

    async def fake_complete(system, user, max_tokens=512):
        seen_concurrency["cur"] += 1
        seen_concurrency["max"] = max(seen_concurrency["max"], seen_concurrency["cur"])
        await asyncio.sleep(0.01)
        seen_concurrency["cur"] -= 1
        if "capped" in user.lower():
            return '{"value": "Yes", "reasoning": "cap present", "quote": "capped at 1,000,000 GBP"}'
        return '{"value": "12 months", "reasoning": "stated term", "quote": "The term is 12 months"}'

    monkeypatch.setattr(llm, "complete", fake_complete)

    async def run():
        events = []
        async for ev in tabular.generate_review(documents, columns, concurrency=2):
            events.append(ev)
        return events

    events = asyncio.run(run())
    cells = [e for e in events if e["type"] == "cell"]
    assert len(cells) == 4, "one cell per (doc × column)"
    assert events[-1]["type"] == "done"
    assert all(c["status"] == "done" for c in cells)

    # yes_no coerced to a bool; citation carries the quote.
    capped = [c for c in cells if c["column_key"] == "capped"]
    assert all(isinstance(c["value"], bool) for c in capped)
    assert any(c["citations"] and c["citations"][0]["quote_text"] for c in capped)

    # Concurrency was bounded at the configured limit.
    assert seen_concurrency["max"] <= 2


def test_generate_review_isolates_a_failing_cell(monkeypatch, tmp_path):
    d = tmp_path / "a.txt"
    d.write_text("content")
    documents = [{"document_id": "doc1", "path": str(d), "mime": "text/plain"}]
    columns = [
        {"key": "ok", "format": "text", "prompt": "ok?"},
        {"key": "boom", "format": "text", "prompt": "boom?"},
    ]

    async def flaky(system, user, max_tokens=512):
        if "boom" in user.lower():
            raise RuntimeError("model exploded")
        return '{"value": "fine"}'

    monkeypatch.setattr(llm, "complete", flaky)

    async def run():
        return [e async for e in tabular.generate_review(documents, columns, concurrency=2)]

    cells = [e for e in asyncio.run(run()) if e["type"] == "cell"]
    statuses = {c["column_key"]: c["status"] for c in cells}
    assert statuses == {"ok": "done", "boom": "error"}


def test_generate_review_ocrs_images(monkeypatch, tmp_path):
    # An image document is routed through the OCR-aware extractor, so the model
    # receives transcribed text (never binary garbage sent to the embedder → 400).
    img = tmp_path / "scan.png"
    img.write_bytes(b"\x89PNG\r\n fake bytes")
    documents = [{"document_id": "d1", "path": str(img), "mime": "image/png"}]
    columns = [{"key": "term", "format": "text", "prompt": "What is the term?"}]

    async def fake_ocr(path, mime=None):
        return [(1, "The term is 12 months.")]

    monkeypatch.setattr(tabular.extract, "extract_pages_ocr", fake_ocr)

    async def fake_complete(system, user, max_tokens=512):
        assert "12 months" in user, "OCR text must reach the model, not raw bytes"
        return '{"value": "12 months"}'

    monkeypatch.setattr(llm, "complete", fake_complete)

    async def run():
        return [e async for e in tabular.generate_review(documents, columns)]

    cells = [e for e in asyncio.run(run()) if e["type"] == "cell"]
    assert len(cells) == 1 and cells[0]["status"] == "done"
    assert cells[0]["column_key"] != "*"


def test_generate_review_blank_doc_emits_single_error_and_no_cells(monkeypatch, tmp_path):
    # A blank/unreadable image (OCR yields nothing) → exactly one whole-doc "*"
    # error and NO per-column cells (never embed empty text; no silent empty cells).
    img = tmp_path / "blank.png"
    img.write_bytes(b"x")
    documents = [{"document_id": "d1", "path": str(img), "mime": "image/png"}]
    columns = [
        {"key": "a", "format": "text", "prompt": "a?"},
        {"key": "b", "format": "text", "prompt": "b?"},
    ]

    async def fake_ocr(path, mime=None):
        return [(1, "   ")]  # OCR produced no readable text

    monkeypatch.setattr(tabular.extract, "extract_pages_ocr", fake_ocr)

    async def boom(system, user, max_tokens=512):
        raise AssertionError("cells must not run for a blank document")

    monkeypatch.setattr(llm, "complete", boom)

    async def run():
        return [e async for e in tabular.generate_review(documents, columns)]

    events = asyncio.run(run())
    cells = [e for e in events if e["type"] == "cell"]
    assert len(cells) == 1
    assert cells[0]["column_key"] == "*" and cells[0]["status"] == "error"
    assert "No extractable text" in cells[0]["error"]
    assert events[-1]["type"] == "done"


def test_relevant_text_picks_top_k(monkeypatch):
    from app import chunker, embeddings

    monkeypatch.setattr(chunker, "chunk_text", lambda text, **k: ["aaa", "bbb", "ccc"])

    async def fake_embed(texts):
        # query + 3 chunks; query aligns with the first chunk only.
        return [[1.0, 0.0], [1.0, 0.0], [0.0, 1.0], [0.0, 1.0]]

    monkeypatch.setattr(embeddings, "embed", fake_embed)
    out = asyncio.run(tabular._relevant_text("long doc text", "query", k=1))
    assert out == "aaa"


def test_select_text_routes_by_mechanism(monkeypatch):
    calls = []

    async def fake_relevant(text, query, k):
        calls.append(("relevant", k))
        return "RAGGED"

    monkeypatch.setattr(tabular, "_relevant_text", fake_relevant)

    # stuff + small doc → whole text, no retrieval.
    assert asyncio.run(tabular._select_text("small", "q", "stuff")) == "small"
    assert calls == []

    # per_document_rag → retrieval.
    assert asyncio.run(tabular._select_text("small", "q", "per_document_rag")) == "RAGGED"
    assert calls and calls[-1][0] == "relevant"

    # stuff + over-budget → retrieval fallback (never silent truncation).
    big = "x" * (tabular.DOC_BUDGET + 1)
    assert asyncio.run(tabular._select_text(big, "q", "stuff")) == "RAGGED"


def test_export_xlsx(tmp_path):
    out = str(tmp_path / "review.xlsx")
    columns = [{"key": "term", "name": "Term"}, {"key": "capped", "name": "Capped?"}]
    rows = [
        {"document": "a.docx", "cells": {"term": "12 months", "capped": True}},
        {"document": "b.docx", "cells": {"term": "24 months", "capped": False}},
    ]
    path = tabular.export_xlsx("My Review", columns, rows, out)
    assert zipfile.is_zipfile(path), "xlsx is a zip"
    with open(path, "rb") as f:
        assert f.read(2) == b"PK"
