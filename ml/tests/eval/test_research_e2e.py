"""Deep Research live E2E (gated): runs the real pipeline against the live
stack (SearXNG + network + LLM + reranker) and asserts the structural
contract — multi-section report, resolving [W#] citations, references section,
citation list aligned with the numbering.

Run: `PAI_RESEARCH_EVAL=1 .venv/Scripts/python -m pytest tests/eval/test_research_e2e.py -q -s`
"""

import asyncio
import os
import pathlib
import re
import sys

import pytest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[2]))

pytestmark = pytest.mark.skipif(
    os.getenv("PAI_RESEARCH_EVAL") != "1",
    reason="research E2E needs the live stack + network (set PAI_RESEARCH_EVAL=1)",
)


def test_research_run_live():
    from app.research import pipeline

    out = asyncio.run(
        pipeline.run(
            "What are the main approaches to running large language models fully "
            "on-premise in 2026, and what are their trade-offs?",
            template_id="exploration",
        )
    )
    report = out["report_md"]
    print(f"\n=== {out['title']} ===\n{report[:2000]}\n…\ncitations: {len(out['citations'])}")

    sections = re.findall(r"^## \d+\. .+$", report, re.MULTILINE)
    assert len(sections) >= 3, "multi-section report"
    cited = set(re.findall(r"\[(W\d+)\]", report.split("## References")[0]))
    assert cited, "inline citations present"
    assert "## References" in report
    # Dense numbering: cited IDs are exactly W1..Wn.
    assert cited == {f"W{i}" for i in range(1, len(cited) + 1)}, "dense contiguous numbering"
    assert len(out["citations"]) == len(cited), "citation list aligns with the references"
    for c in out["citations"]:
        assert c["url"].startswith("http")


def _write_corpus(tmp: pathlib.Path) -> list[dict]:
    docs = {
        "vector-db-note.txt": (
            "Internal note on self-hosted vector databases. We benchmarked Qdrant "
            "and Milvus on a single GPU host. Qdrant's hybrid search (dense + BM25) "
            "matched our needs; reindexing 2M chunks took 40 minutes. Open question: "
            "does Milvus scale better past 10M vectors?"
        ),
        "inference-note.txt": (
            "Internal note on on-prem inference. vLLM on a single A100 served Qwen3 at "
            "acceptable latency; llama.cpp suited the Mac Studio profile. We did not "
            "test multi-node serving. Cost was dominated by GPU amortisation."
        ),
    }
    out = []
    for i, (name, body) in enumerate(docs.items(), start=1):
        p = tmp / name
        p.write_text(body, encoding="utf-8")
        out.append({
            "doc_id": f"00000000-0000-0000-0000-00000000000{i}",
            "kb_id": "00000000-0000-0000-0000-0000000000aa",
            "kb_name": "Internal notes",
            "path": str(p),
            "mime": "text/plain",
            "filename": name,
        })
    return out


def test_research_files_run_live(tmp_path):
    """Corpus census over a tiny local corpus — ZERO egress. Asserts the [D#]
    namespace, document references, and the honest coverage appendix."""
    from app.research import pipeline

    docs = _write_corpus(tmp_path)
    out = asyncio.run(
        pipeline.run(
            "What do our internal notes conclude about self-hosting LLM "
            "infrastructure, and what did we not test?",
            template_id="formal",
            source="files",
            kb_ids=["00000000-0000-0000-0000-0000000000aa"],
            docs=docs,
            total_docs=len(docs),
        )
    )
    report = out["report_md"]
    print(f"\n=== {out['title']} ===\n{report[:2000]}\n…\ndoc_citations: {len(out['doc_citations'])}")
    assert re.search(r"\[D\d+\]", report), "document citations present"
    assert "[W" not in report.split("## References")[0], "files-only ⇒ no web markers"
    assert "## Coverage" in report
    assert out["citations"] == [], "no web citations on a files-only run"
    assert out["doc_citations"], "document citations returned for persistence"
