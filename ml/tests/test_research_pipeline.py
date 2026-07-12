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

"""End-to-end pipeline with mocked collect + LLM: dense W1..Wn renumbering in
first-appearance order, citation-list alignment, beast-mode delivery, honest
empty-evidence answer, and never-raises on total LLM failure."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

import asyncio
import re

from app import llm, reranker
from app.config import settings
from app.research import pipeline as rp
from app.web import loop as web_loop
from app.web.loop import CollectResult, _Pool
from app.web.pipeline import _Source


def _run(coro):
    return asyncio.new_event_loop().run_until_complete(coro)


def _mk_source(i: int) -> _Source:
    return _Source(
        url=f"https://s{i}.example/a", title=f"Source {i}", domain=f"s{i}.example",
        published_date="2026-05-01", fetched_at="2026-06-10T00:00:00+00:00",
        snippet_only=False, chunks=[f"evidence text from source {i} " * 8],
    )


def _mock_stack(monkeypatch, *, n_sources: int = 3, writer_cites: list[str] | None = None):
    """Mock _resolve_model, web collect, reranker and the LLM. The writer cites
    `writer_cites` (default ['W3', 'W1']) so renumbering is observable."""

    async def fake_resolve():
        return ("model", 65_536)

    import app.main as main_mod

    monkeypatch.setattr(main_mod, "_resolve_model", fake_resolve)

    async def fake_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        pool = pool if pool is not None else _Pool()
        for i in range(1, n_sources + 1):
            src = _mk_source(i)
            if src.url not in {s.url for s in pool.sources}:
                pool.upgrade_fetched(src)
        return CollectResult(pool=pool, subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", fake_collect)

    async def fake_rerank(query, docs):
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)

    cites = writer_cites if writer_cites is not None else ["W3", "W1"]
    body = ("substantive finding " * 25).strip()

    async def fake_llm(system, user, max_tokens=0):
        if "Decompose the research question" in system:
            return '["the question", "a follow-up angle"]'
        if "research notes from ONE web source" in system:
            return '{"claims": ["a precise claim with a figure of 42"], "quotes": ["verbatim forty-two"]}'
        if "planning a research report outline" in system:
            return (
                '[{"heading": "Findings", "brief": "what we found", "note_ids": ["W1","W2","W3"]},'
                '{"heading": "Analysis", "brief": "what it means", "note_ids": ["W2"]},'
                '{"heading": "Risks", "brief": "the risks", "note_ids": ["W1"]},'
                '{"heading": "Conclusions", "brief": "where it lands", "note_ids": ["W3"]}]'
            )
        if "EDITING a finished research report" in system:
            return user.split("\n\n", 1)[1]  # echo the draft unchanged
        if "running summary" in system:
            return "summary so far"
        if "report title" in system:
            return "A Crisp Research Title"
        # Section writer: cite the configured IDs.
        marks = " ".join(f"[{c}]" for c in cites)
        return f"{body} {marks}"

    monkeypatch.setattr(llm, "complete", fake_llm)


def test_dense_renumbering_and_citation_alignment(monkeypatch):
    _mock_stack(monkeypatch, writer_cites=["W3", "W1"])
    out = _run(rp.run("the question", "freeform"))
    assert out["title"] == "A Crisp Research Title"
    report = out["report_md"]
    # First-appearance order: W3 (first cited) becomes W1, W1 becomes W2.
    used = re.findall(r"\[(W\d+)\]", report.split("## References")[0])
    assert set(used) == {"W1", "W2"}, f"dense contiguous IDs, got {set(used)}"
    assert used[0] == "W1"
    # References section lists exactly the cited sources, in order.
    refs = report.split("## References")[1]
    assert "[W1] Source 3" in refs, "old W3 is the first-cited → new W1"
    assert "[W2] Source 1" in refs
    assert "Source 2" not in refs, "uncited sources stay out of the references"
    # Citations list aligns with reference order.
    assert [c["url"] for c in out["citations"]] == ["https://s3.example/a", "https://s1.example/a"]
    assert out["citations"][0]["quote_text"] == "verbatim forty-two"


def test_empty_evidence_honest_report(monkeypatch):
    _mock_stack(monkeypatch, n_sources=0)
    out = _run(rp.run("q", "exploration"))
    assert out["citations"] == []
    assert "No usable web sources" in out["report_md"]


def test_immediate_deadline_returns_honest_empty(monkeypatch):
    # Deadline already expired ⇒ collection never starts ⇒ no sources ⇒ the
    # honest empty-evidence report (not a fabricated one).
    _mock_stack(monkeypatch)
    monkeypatch.setattr(settings, "research_max_minutes", 0.0)
    out = _run(rp.run("q", "formal"))
    assert "No usable web sources" in out["report_md"]
    assert out["citations"] == []


def test_beast_mode_after_collection_delivers(monkeypatch):
    # The collection budget expires after the first sub-question: the run
    # flags beast mode, still synthesises from what it gathered, and appends
    # the honest budget note. (The global `time` module cannot be patched —
    # asyncio's event loop runs on it — so expire the collect state instead.)
    _mock_stack(monkeypatch)
    calls = {"n": 0}

    def fake_expired(self):
        calls["n"] += 1
        return calls["n"] > 1  # first check passes, every later one is expired

    monkeypatch.setattr(web_loop._State, "expired", fake_expired)
    out = _run(rp.run("q", "exploration"))
    assert "best-effort" in out["report_md"], "beast-mode note present"
    assert "## 1." in out["report_md"], "sections still written from gathered evidence"


def test_total_llm_failure_never_raises(monkeypatch):
    _mock_stack(monkeypatch)

    async def dead(system, user, max_tokens=0):
        raise RuntimeError("LLM down")

    monkeypatch.setattr(llm, "complete", dead)
    out = _run(rp.run("the question", "exploration"))
    assert out["report_md"], "degraded but delivered"
    assert out["title"], "title falls back to the question"


def test_formal_template_headings_present(monkeypatch):
    _mock_stack(monkeypatch)
    out = _run(rp.run("q", "formal"))
    r = out["report_md"]
    for h in ["Executive summary", "Background", "Findings", "Analysis", "Conclusions & recommendations"]:
        assert h in r, f"formal skeleton heading '{h}' present"
    assert "[[EXECUTIVE-SUMMARY]]" not in r, "placeholder never ships"


def test_web_only_returns_empty_doc_citations(monkeypatch):
    _mock_stack(monkeypatch)
    out = _run(rp.run("q", "freeform"))
    assert out["doc_citations"] == [], "a web run carries no document citations"


# --- W-only renumber byte-identity (Phase-1 regression guard) ----------------


def test_renumber_w_only_dense_first_appearance():
    from app.research.bank import Bank

    b = Bank()
    for i in range(1, 6):
        b.add_source(_mk_source(i))  # W1..W5
    draft = "Alpha [W5] beta [W2] gamma [W5]."
    report, web, doc = rp._renumber(draft, b)
    assert report == "Alpha [W1] beta [W2] gamma [W1].", "first-appearance dense W renumber"
    assert [r.sid for r in web] == ["W5", "W2"], "cited records in first-appearance order"
    assert doc == [], "no document namespace in a web report"


# --- Corpus + hybrid -------------------------------------------------------


def _docs(n: int) -> list[dict]:
    return [
        {"doc_id": f"doc-{i}", "kb_id": "kb1", "kb_name": "Contracts",
         "filename": f"file{i}.docx", "path": f"/x/{i}", "mime": None}
        for i in range(1, n + 1)
    ]


def _mock_corpus_stack(monkeypatch, *, doc_cites, web_cites=None, with_web=False):
    """Mock _resolve_model, census, reranker, corpus analysis and the LLM for a
    corpus (or hybrid) run. The writer cites `doc_cites` (and `web_cites`)."""
    import app.main as main_mod
    from app.research import bank as bank_mod
    from app.research import census as census_mod
    from app.research import corpus_analysis as corpus_mod

    async def fake_resolve():
        return ("model", 65_536)

    monkeypatch.setattr(main_mod, "_resolve_model", fake_resolve)

    async def fake_census(docs, bank, b, deadline, model_id):
        for d in docs:
            sid = bank.add_doc_source(
                bank_mod.DocSource(doc_id=d["doc_id"], kb_id=d["kb_id"],
                                   kb_name=d["kb_name"], filename=d["filename"])
            )
            bank.get(sid).note = bank_mod.Note(
                claims=[f"a claim from {d['filename']}"], quotes=[f"quote {d['doc_id']}"]
            )
        return census_mod.CensusResult(reviewed=len(docs), unreviewed=[], stuffed_corpus=False)

    monkeypatch.setattr(census_mod, "run_census", fake_census)

    called = {"web": False}

    async def fake_collect(query, recency, budget, *, pool=None, seen=None, state=None):
        called["web"] = True
        pool = pool if pool is not None else _Pool()
        for i in range(1, 3):
            src = _mk_source(i)
            if src.url not in {s.url for s in pool.sources}:
                pool.upgrade_fetched(src)
        return CollectResult(pool=pool, subq_evidence=[], notes=[], beast=False)

    monkeypatch.setattr(web_loop, "collect", fake_collect)

    async def fake_rerank(query, docs):
        return [float(len(docs) - i) for i in range(len(docs))]

    monkeypatch.setattr(reranker, "rerank", fake_rerank)

    body = ("substantive corpus finding " * 25).strip()
    cites = [*doc_cites, *(web_cites or [])]

    async def fake_llm(system, user, max_tokens=0):
        if "Decompose the research question" in system or "fill the GAPS" in system:
            return '["the question", "a follow-up angle"]'
        if "analysing a corpus" in system:
            return '{"consensus": [{"point": "they agree on X", "sids": ["D1"]}],' \
                   ' "contradictions": [{"point": "they clash on Y", "sids": ["D1","D2"]}],' \
                   ' "gaps": ["nothing covers Z"]}'
        if "planning a research report outline" in system:
            return ('[{"heading": "Findings", "brief": "b", "note_ids": ["D1","D2"]},'
                    '{"heading": "Analysis", "brief": "b", "note_ids": ["D2"]},'
                    '{"heading": "Conclusions", "brief": "b", "note_ids": ["D1"]}]')
        if "EDITING a finished research report" in system:
            return user.split("\n\n", 1)[1]
        if "running summary" in system:
            return "summary so far"
        if "report title" in system:
            return "A Corpus Title"
        marks = " ".join(f"[{c}]" for c in cites)
        return f"{body} {marks}"

    monkeypatch.setattr(llm, "complete", fake_llm)
    return called


def test_files_only_no_web_dual_namespace_and_coverage(monkeypatch):
    called = _mock_corpus_stack(monkeypatch, doc_cites=["D1", "D2"])
    out = _run(rp.run("q", "exploration", source="files",
                      kb_ids=["kb1"], docs=_docs(3), total_docs=3))
    assert called["web"] is False, "files-only performs ZERO web collection"
    r = out["report_md"]
    assert "[D1]" in r and "[D2]" in r, "document citations present"
    assert "[W" not in r.split("## References")[0], "no web markers in a files run"
    # References: documents only (flat — no web subheading).
    refs = r.split("## References")[1]
    assert "[D1] file" in refs
    assert "### Web sources" not in refs
    # Honest coverage appendix + contradictions section.
    assert "## Coverage" in r and "All 3 documents" in r
    assert "Consensus, contradictions and gaps" in r
    # Document citations returned for persistence (page anchors None for census).
    assert len(out["doc_citations"]) == 2
    assert out["doc_citations"][0]["page_number"] is None
    assert out["citations"] == []


def test_hybrid_merges_both_namespaces_segregated(monkeypatch):
    called = _mock_corpus_stack(monkeypatch, doc_cites=["D1", "D2"], web_cites=["W1"], with_web=True)
    out = _run(rp.run("q", "formal", source="hybrid",
                      kb_ids=["kb1"], docs=_docs(2), total_docs=2))
    assert called["web"] is True, "hybrid fills gaps from the web"
    r = out["report_md"]
    refs = r.split("## References")[1]
    assert "### Your documents" in refs and "### Web sources" in refs, "provenance segregated"
    assert out["doc_citations"], "document citations persisted"
    assert out["citations"], "web citations persisted"


def test_verify_off_is_byte_identical_and_no_verification(monkeypatch):
    # The Phase-3 regression guard: with verify=False (the default) the report is
    # exactly what Phase 2 produced and `verification` is None.
    _mock_stack(monkeypatch, writer_cites=["W3", "W1"])
    base = _run(rp.run("the question", "freeform"))
    assert base["verification"] is None, "unverified runs carry no verification"
    _mock_stack(monkeypatch, writer_cites=["W3", "W1"])
    explicit_off = _run(rp.run("the question", "freeform", verify=False))
    assert explicit_off["report_md"] == base["report_md"], "verify=False ⇒ byte-identical"


def test_verify_on_cuts_contradicted_and_returns_summary(monkeypatch):
    _mock_stack(monkeypatch, writer_cites=["W1"])
    # Enable the verifier; stub decompose/locate/verify so [W1] sentences are
    # contradicted → cut, and the summary is populated.
    from app import decompose, locate
    from app import verify as verify_svc
    monkeypatch.setattr(settings, "verify_enabled", True)

    async def fake_decompose(text):
        import re as _re
        return [s.strip() for s in _re.split(r"(?<=[.!?])\s+", text.strip()) if s.strip()]
    monkeypatch.setattr(decompose, "decompose_claims", fake_decompose)
    monkeypatch.setattr(locate, "locate", lambda c, body, hint_start=0, min_cover=0.5:
                        ({"start": body.find(c), "end": body.find(c) + len(c), "text": c} if c in body else None))

    async def verdicts(pairs, hhem_filter=False):
        return [{"verdict": "contradicted", "score": 0.9} for _ in pairs]
    monkeypatch.setattr(verify_svc, "verify_claims", verdicts)

    from app import embeddings
    async def fake_embed(texts):
        return [[0.0, 1.0] for _ in texts]  # orthogonal-ish; no padding flag, no network
    monkeypatch.setattr(embeddings, "embed", fake_embed)

    out = _run(rp.run("the question", "freeform", verify=True))
    assert out["verification"] is not None, "a verified run carries a summary"
    assert out["verification"]["contradicted"] >= 1
    # Spans resolve against the final report (offsets valid).
    for s in out["verification"]["spans"]:
        assert out["report_md"][s["start"]:s["end"]] == s["text"]


def test_above_cap_falls_back_to_sampling(monkeypatch):
    # total_docs over the cap ⇒ retrieval sampling, not census.
    _mock_corpus_stack(monkeypatch, doc_cites=["D1"])
    import app.retrieve as retrieve_mod

    async def fake_retrieve(prompt, kb_ids):
        return {"context": "", "citations": [
            {"doc_id": "doc-7", "chunk_index": 3, "page_number": 9,
             "clause_section_ref": "§2", "quote_text": "sampled passage"},
        ]}

    monkeypatch.setattr(retrieve_mod, "retrieve", fake_retrieve)
    monkeypatch.setattr(settings, "research_census_cap", 2)
    out = _run(rp.run("q", "exploration", source="files",
                      kb_ids=["kb1"], docs=_docs(2), total_docs=500))
    assert "## Coverage" in out["report_md"]
    assert "sampling" in out["report_md"].lower(), "honest sampling statement"
