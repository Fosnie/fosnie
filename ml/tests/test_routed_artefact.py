# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
import base64
import hashlib
import io
import sys
from types import SimpleNamespace

import pytest
from pydantic import ValidationError
from app import routed_artefact as renderer


@pytest.mark.parametrize("kind", ["md", "docx"])
def test_offline_render_preserves_draft_without_legacy_pipeline(monkeypatch, kind):
    from app import generate, llm
    monkeypatch.setattr(generate, "generate_artefact", lambda *a, **k: pytest.fail("legacy renderer"))
    monkeypatch.setattr(llm, "chat_step", lambda *a, **k: pytest.fail("unplanned inference"))
    request = renderer.RenderRequest(schema_version=1, kind=kind, title="Draft", content="A documented answer.\n\n![external](https://unreachable.invalid/a.png)")
    result = renderer.render(request)
    data = base64.b64decode(result["bytes_base64"])
    assert result["sha256"] == hashlib.sha256(data).hexdigest()
    assert result["mime"] == renderer.MIMES[kind]
    if kind == "docx":
        import docx
        document = docx.Document(io.BytesIO(data))
        assert "A documented answer." in "\n".join(p.text for p in document.paragraphs)
        assert len(document.inline_shapes) == 0
        assert not any(r.is_external for r in document.part.rels.values())
    else:
        assert data.startswith(b"# Draft\n\nA documented answer.")


def test_pdf_uses_resource_deny_and_escapes_raw_html(monkeypatch):
    from app import validators
    paths = []
    class HTML:
        def __init__(self, *, string, url_fetcher):
            assert "<img" not in string
            assert "&lt;img" in string
            for url in ["https://example.invalid/image", "file:///etc/passwd", "data:image/png;base64,AAAA"]:
                with pytest.raises(ValueError, match="cannot load"):
                    url_fetcher(url)
        def write_pdf(self, path):
            from pathlib import Path
            paths.append(Path(path))
            Path(path).write_bytes(b"%PDF-synthetic")
    monkeypatch.setitem(sys.modules, "weasyprint", SimpleNamespace(HTML=HTML))
    monkeypatch.setattr(validators, "validate_pdf", lambda path: None)
    result = renderer.render(renderer.RenderRequest(schema_version=1,kind="pdf",title="Draft",content='<img src="file:///etc/passwd">\n\n![x](https://example.invalid/x)'))
    assert base64.b64decode(result["bytes_base64"]).startswith(b"%PDF-")
    assert paths and not paths[0].exists(), "temporary output removed before returning"


@pytest.mark.parametrize("extra", [{"out_path":"/tmp/steal"}, {"overrides":{}}, {"schema_version":2}, {"kind":"html"}, {"content":" "}, {"content":"я"*600000}])
def test_strict_contract_rejects_paths_overrides_formats_and_unbounded_text(extra):
    value = {"schema_version":1,"kind":"md","title":"Draft","content":"text"}
    value.update(extra)
    with pytest.raises(ValidationError):
        renderer.RenderRequest(**value)
