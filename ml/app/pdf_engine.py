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

"""Beautiful PDF via Markdown→HTML→WeasyPrint against a print stylesheet — the
primary PDF route. WeasyPrint implements CSS Paged Media
(@page headers/footers, page counters, running strings), giving designed,
paginated PDFs without a browser, fully offline (zero-egress).

`available()` gates the route: WeasyPrint needs native libraries (Pango/cairo/
GDK-PixBuf), so where they are absent (e.g. the Windows dev box) the caller falls
back to the LibreOffice DOCX→PDF path. markdown-it-py (pure Python) renders the
HTML; the stylesheet is the bundled neutral default or a deployment's brand CSS
(`pdf_css`)."""

from __future__ import annotations

import html as _html
import re
from pathlib import Path

from .config import settings

_DEFAULT_CSS = Path(__file__).parent / "assets" / "pdf" / "report.css"


def available() -> bool:
    """True only if WeasyPrint imports cleanly (its native deps are present)."""
    try:
        import weasyprint  # noqa: F401
    except Exception:
        return False
    return True


def _css_text() -> str:
    p = Path(settings.pdf_css) if settings.pdf_css else _DEFAULT_CSS
    if not p.is_file():
        p = _DEFAULT_CSS
    css = p.read_text(encoding="utf-8")
    # Strip CSS block comments before inlining: the stylesheet's Apache licence header
    # (and any commented URL) must never ship in the rendered HTML — it would break the
    # self-contained/zero-egress guarantee and clutter every user PDF. Comments carry no
    # rendering value, so dropping them is safe (and trims bytes).
    return re.sub(r"/\*.*?\*/", "", css, flags=re.S).strip()


def _compose_markdown(content: str, title: str) -> str:
    body = content.lstrip()
    if title and not body.startswith("# "):
        return f"# {title}\n\n{content}"
    return content


def render_html(content: str, title: str) -> str:
    """Markdown → a single self-contained HTML document (stylesheet inlined, no
    external URLs). Raw HTML in the Markdown is escaped (`html=False`)."""
    from markdown_it import MarkdownIt

    md = MarkdownIt("commonmark", {"html": False}).enable(["table", "strikethrough"])
    body_html = md.render(_compose_markdown(content, title))
    safe_title = _html.escape(title or "Document")
    css = _css_text()
    return (
        "<!DOCTYPE html>\n<html lang=\"en\"><head><meta charset=\"utf-8\">"
        f"<title>{safe_title}</title><style>{css}</style></head>"
        f"<body><main class=\"report\">{body_html}</main></body></html>"
    )


def md_to_pdf(content: str, title: str, out_path: str) -> str:
    """Render `content` (Markdown) to a styled PDF at `out_path`. Raises
    RuntimeError if WeasyPrint is unavailable or rendering fails."""
    try:
        from weasyprint import HTML
    except Exception as e:  # native libs missing
        raise RuntimeError(f"WeasyPrint is not available: {e}") from e

    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    doc_html = render_html(content, title)
    HTML(string=doc_html).write_pdf(out_path)
    if not Path(out_path).is_file():
        raise RuntimeError("WeasyPrint produced no output")
    return out_path
