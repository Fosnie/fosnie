"""Fixed-eval-set check for L3 contextual retrieval (chunker-strategy §8).

Gated on PAI_EVAL=1 — needs the real stack (Qdrant + embeddings + the local LLM
for blurb generation). Ingests a small corpus whose answer chunk is only
*implicitly* about the subject (a bare pronoun: "It may be terminated …"), buried
among distractors so top-k cannot return everything. With contextual retrieval
ON, the situating blurb pulls the subject ("Acme … MSA … termination") into the
embedded text, so the answer chunk is retrieved; with it OFF it tends to miss.

Run: `PAI_EVAL=1 .venv/Scripts/python -m pytest tests/eval/ -q`
"""

import asyncio
import os
import pathlib
import sys
import uuid

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))

pytestmark = pytest.mark.skipif(
    os.getenv("PAI_EVAL") != "1", reason="eval set requires the real stack (set PAI_EVAL=1)"
)

ANSWER = "It may be terminated by either party upon ninety days written notice."
PARTIES = "This Master Services Agreement (the MSA) is made between Acme Corp and Beta LLC in 2024."
DISTRACTORS = [
    "The supplier shall deliver the goods to the named warehouse.",
    "Invoices are payable in pounds sterling within thirty days.",
    "Confidential information must be protected using reasonable measures.",
    "The governing law of this document is the law of England and Wales.",
    "Each party shall appoint a relationship manager for the engagement.",
    "Force majeure suspends obligations affected by the qualifying event.",
    "Intellectual property created remains owned by its originating party.",
    "Warranties are limited to the express terms set out in the schedule.",
    "Assignment requires the prior written consent of the other party.",
    "Notices shall be sent to the registered office of each party.",
    "The schedule lists the agreed service levels and credits.",
    "Subcontracting does not relieve the supplier of its obligations.",
    "Amendments are effective only when signed by both parties.",
    "Data is processed in accordance with applicable data-protection law.",
]


def _make_corpus(tmp: pathlib.Path) -> str:
    # One paragraph per line-group; small chunk size makes each its own chunk.
    body = "\n\n".join([PARTIES, *DISTRACTORS, ANSWER])
    p = tmp / "msa.txt"
    p.write_text(body, encoding="utf-8")
    return str(p)


async def _ingest_and_retrieve(path: str, contextual_on: bool) -> dict:
    from app import embeddings, ingest, qdrant_store
    from app import retrieve as retrieve_mod
    from app.config import settings

    settings.parent_child = False
    settings.contextual_retrieval = contextual_on
    settings.chunk_size = 130  # each short paragraph → its own chunk
    settings.chunk_overlap = 0

    kb_id = uuid.uuid4().hex
    doc_id = uuid.uuid4().hex
    dim = await embeddings.dimension()
    await ingest.ingest_document(doc_id, kb_id, path, "text/plain", dim)
    try:
        return await retrieve_mod.retrieve("how can the Acme MSA be terminated?", [kb_id])
    finally:
        # Single shared collection — purge just this doc's chunks, don't drop it.
        await qdrant_store.delete_doc(kb_id, doc_id)


def _hit(result: dict) -> bool:
    return any("terminated" in c["quote_text"].lower() for c in result["citations"]) or (
        "terminated" in result["context"].lower()
    )


def test_contextual_retrieval_surfaces_implicit_chunk(tmp_path):
    path = _make_corpus(tmp_path)
    on = asyncio.run(_ingest_and_retrieve(path, contextual_on=True))
    assert _hit(on), "with contextual retrieval ON, the termination chunk should be retrieved"

    # The OFF run is reported for signal but not asserted (model-dependent).
    off = asyncio.run(_ingest_and_retrieve(path, contextual_on=False))
    print(f"[eval] termination chunk retrieved — contextual ON: {_hit(on)}, OFF: {_hit(off)}")
