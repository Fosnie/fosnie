"""Generated-artefact tests: md/docx always; pdf only if LibreOffice present."""

import zipfile

import docx
import pytest

from app import generate, render


def test_generate_md(tmp_path):
    out = str(tmp_path / "a.md")
    res = generate.generate_artefact("md", "My Title", "Hello world.\n\nSecond para.", out)
    assert res["mime"] == "text/markdown"
    text = open(res["path"], encoding="utf-8").read()
    assert "# My Title" in text and "Second para." in text


def test_generate_docx(tmp_path):
    out = str(tmp_path / "a.docx")
    res = generate.generate_artefact("docx", "Contract", "Clause one.\n\nClause two.", out)
    assert zipfile.is_zipfile(res["path"])
    d = docx.Document(res["path"])
    body = "\n".join(p.text for p in d.paragraphs)
    assert "Contract" in body and "Clause one." in body and "Clause two." in body


def test_generate_unknown_kind(tmp_path):
    with pytest.raises(ValueError):
        generate.generate_artefact("xls", "t", "c", str(tmp_path / "x.xls"))


@pytest.mark.skipif(not render.available(), reason="LibreOffice not installed")
def test_generate_pdf(tmp_path):
    out = str(tmp_path / "a.pdf")
    res = generate.generate_artefact("pdf", "Brief", "Body text here.", out)
    assert res["mime"] == "application/pdf"
    with open(res["path"], "rb") as f:
        assert f.read(5) == b"%PDF-"
