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

"""Self-contained HTML artefacts. The model writes a page with placeholder markers; this engine turns it
into a true single file that opens offline anywhere:

- `<!-- pai:echarts -->` → the vendored Apache ECharts build inlined in a <script>
  (the model never spends tokens on library code; charts work with no CDN).
- `<!-- pai:theme -->`   → the neutral (or deployment-branded) theme inlined in a
  <style>.
- a strict CSP `<meta>` is injected into <head> if absent — `default-src 'none'`
  with no `connect-src`, so even a malicious inline script in the sandboxed preview
  cannot fetch/XHR/WebSocket out. This is the in-artefact zero-egress guarantee; it
  travels in the bytes, so a script inside cannot remove it.

`generate.py` validates the result (`validators.validate_html`) before it ships."""

from __future__ import annotations

import re
from pathlib import Path

from .config import settings

HTML_MIME = "text/html"

_ASSETS = Path(__file__).parent / "assets"
_ECHARTS = _ASSETS / "vendor" / "echarts.min.js"
_DEFAULT_THEME = _ASSETS / "html" / "theme.css"

# Strict policy injected into every artefact. No `connect-src`/`default-src` egress;
# inline script/style allowed (the page is fully static + sandboxed at null origin).
_CSP = (
    "default-src 'none'; "
    "script-src 'unsafe-inline'; "
    "style-src 'unsafe-inline'; "
    "img-src data:; "
    "font-src data:; "
    "base-uri 'none'; "
    "form-action 'none'"
)
_CSP_META = f'<meta http-equiv="Content-Security-Policy" content="{_CSP}">'

_HAS_HTML = re.compile(r"<html[\s>]", re.IGNORECASE)
_HAS_CSP = re.compile(r'http-equiv\s*=\s*["\']?content-security-policy', re.IGNORECASE)
_HEAD_OPEN = re.compile(r"<head[^>]*>", re.IGNORECASE)
_ECHARTS_MARKER = re.compile(r"<!--\s*pai:echarts\s*-->", re.IGNORECASE)
_THEME_MARKER = re.compile(r"<!--\s*pai:theme\s*-->", re.IGNORECASE)


def _theme_css() -> str:
    p = Path(settings.html_theme) if settings.html_theme else _DEFAULT_THEME
    if not p.is_file():
        p = _DEFAULT_THEME
    return p.read_text(encoding="utf-8") if p.is_file() else ""


def _wrap_document(content: str, title: str) -> str:
    safe_title = (
        title.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;") or "Page"
    )
    return (
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">"
        f"<title>{safe_title}</title></head><body>{content}</body></html>"
    )


def _inject_into_head(doc: str, snippet: str) -> str:
    """Insert `snippet` right after the opening <head> tag (or after <html>, or
    prepend) so injected policy/styles sit before the page's own."""
    m = _HEAD_OPEN.search(doc)
    if m:
        return doc[: m.end()] + snippet + doc[m.end() :]
    return snippet + doc


def build(content: str, title: str, out_path: str) -> dict:
    """Inline vendored libraries + theme at their markers, inject the CSP, and write
    a single self-contained HTML file. Returns {path, mime}."""
    doc = content if _HAS_HTML.search(content) else _wrap_document(content, title)

    # Theme — only where the page asks for it.
    if _THEME_MARKER.search(doc):
        doc = _THEME_MARKER.sub(lambda _m: f"<style>\n{_theme_css()}\n</style>", doc)

    # ECharts — inline the vendored build at the marker. Marker present but the
    # vendored file missing is a deploy error: fail loudly, never ship a CDN link.
    if _ECHARTS_MARKER.search(doc):
        if not _ECHARTS.is_file():
            raise RuntimeError(
                "page requests ECharts but the vendored build is missing "
                f"({_ECHARTS}); vendor echarts.min.js"
            )
        echarts_js = _ECHARTS.read_text(encoding="utf-8")
        doc = _ECHARTS_MARKER.sub(lambda _m: f"<script>\n{echarts_js}\n</script>", doc)

    # CSP — always present in the served bytes.
    if not _HAS_CSP.search(doc):
        doc = _inject_into_head(doc, _CSP_META)

    out = Path(out_path)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(doc, encoding="utf-8")
    return {"path": str(out), "mime": HTML_MIME}
