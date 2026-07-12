"""L3 contextual-retrieval helpers: augment is pure; contextualise is best-effort."""

import asyncio

from app import contextual, llm


def test_augment_preserves_original_after_blurb():
    out = contextual.augment("This is the indemnity clause of the 2024 MSA.", "The vendor shall indemnify.")
    assert out.endswith("The vendor shall indemnify."), "original chunk kept verbatim"
    assert out.startswith("This is the indemnity clause"), "blurb prepended"
    # Empty blurb → just the chunk (no leading whitespace).
    assert contextual.augment("", "bare chunk") == "bare chunk"
    assert contextual.augment("   ", "bare chunk") == "bare chunk"


def test_contextualise_returns_blurb(monkeypatch):
    async def fake_complete(system, user, max_tokens=80):
        assert "<document>" in user and "Chunk:" in user
        return "  This clause sits in the termination section.  "

    monkeypatch.setattr(llm, "complete", fake_complete)
    blurb = asyncio.run(contextual.contextualise("FULL DOC TEXT", "the chunk"))
    assert blurb == "This clause sits in the termination section."


def test_contextualise_degrades_on_error(monkeypatch):
    async def boom(system, user, max_tokens=80):
        raise RuntimeError("llm down")

    monkeypatch.setattr(llm, "complete", boom)
    assert asyncio.run(contextual.contextualise("doc", "chunk")) == ""
