"""Cross-platform LibreOffice (soffice) resolution for render.py. No LibreOffice
needed — exercises the resolver's fallback logic in isolation. Run:
`uv run pytest tests/test_render_resolve.py` from ml/."""

import os
import pathlib
import sys
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app import render  # noqa: E402
from app.config import settings  # noqa: E402


def test_macos_fallback_present():
    # Regression guard: the Mac Studio profile path must be probed.
    assert "/Applications/LibreOffice.app/Contents/MacOS/soffice" in render._FALLBACKS
    assert any(p.startswith("/usr/") for p in render._FALLBACKS), "a Linux path too"


def test_resolves_none_when_absent(monkeypatch):
    # Isolate from any LibreOffice actually installed on the dev box.
    monkeypatch.setattr(render, "_FALLBACKS", [])
    monkeypatch.setattr(render, "_FALLBACK_GLOBS", [])
    monkeypatch.setattr(settings, "soffice_bin", "pai-no-such-soffice-xyz")
    assert render._resolve_bin() is None
    assert render.available() is False  # graceful (caller returns HTTP 503)


def test_resolves_explicit_fallback(monkeypatch):
    with tempfile.NamedTemporaryFile(suffix="-soffice", delete=False) as f:
        path = f.name
    monkeypatch.setattr(settings, "soffice_bin", "pai-no-such-soffice-xyz")
    monkeypatch.setattr(render, "_FALLBACKS", [path])
    monkeypatch.setattr(render, "_FALLBACK_GLOBS", [])
    try:
        assert render._resolve_bin() == path
        assert render.available() is True
    finally:
        os.unlink(path)
