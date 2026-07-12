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

"""Beautiful DOCX via pandoc against a styled *reference document* — the primary
DOCX route. This is the swappable-engine choice the
Decided list anticipated: pandoc renders the model's Markdown into real Word
styles taken from `reference.docx` (every Heading/Body/Table/TOC style lives in
that one file — the highest-leverage knob for brand-perfect output).

Mirrors `render.py`'s shape: `available()` gates the route so the platform
degrades cleanly to the structural python-docx builder where pandoc is absent
(both deployment profiles keep working). A neutral reference is generated on first
use; a deployment points `docx_reference` at its own branded file."""

from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

from .config import settings

# Common pandoc locations checked when not on PATH. `pandoc_bin` (config) wins.
_FALLBACKS = [
    "/usr/bin/pandoc",
    "/usr/local/bin/pandoc",
    "/opt/homebrew/bin/pandoc",  # macOS (Apple silicon)
    r"C:\Program Files\Pandoc\pandoc.exe",
    str(Path.home() / "AppData" / "Local" / "Pandoc" / "pandoc.exe"),
]


def _resolve_bin() -> str | None:
    found = shutil.which(settings.pandoc_bin)
    if found:
        return found
    for cand in _FALLBACKS:
        if Path(cand).is_file():
            return cand
    return None


def available() -> bool:
    return _resolve_bin() is not None


def ensure_reference_docx() -> str:
    """Return the path to the brand reference.docx, generating a neutral one if the
    configured file is absent."""
    ref = Path(settings.docx_reference)
    if ref.is_file():
        return str(ref)
    ref.parent.mkdir(parents=True, exist_ok=True)
    _build_neutral_reference(ref)
    return str(ref)


def _build_neutral_reference(path: Path) -> None:
    """A clean neutral reference: python-docx's default template already defines
    Heading 1–9, Title, Normal and a Table Grid style; we tune type so output reads
    as professionally designed rather than Word-default. Not PAI-branded — a
    deployment supplies its own reference.docx to brand."""
    import docx
    from docx.shared import Pt, RGBColor

    d = docx.Document()

    normal = d.styles["Normal"]
    normal.font.name = "Calibri"
    normal.font.size = Pt(11)

    heading_ink = RGBColor(0x11, 0x18, 0x27)
    for name, size in (("Title", 26), ("Heading 1", 18), ("Heading 2", 14), ("Heading 3", 12), ("Heading 4", 11)):
        try:
            st = d.styles[name]
            st.font.name = "Calibri"
            st.font.size = Pt(size)
            st.font.color.rgb = heading_ink
            st.font.bold = True
        except KeyError:
            pass

    d.save(str(path))


def _compose_markdown(content: str, title: str) -> str:
    """Prepend the title as an H1 unless the content already opens with one (DR
    conversions pass content that already begins `# Title`)."""
    body = content.lstrip()
    if title and not body.startswith("# "):
        return f"# {title}\n\n{content}\n"
    return content if content.endswith("\n") else content + "\n"


def md_to_docx(content: str, title: str, out_path: str) -> str:
    """Render `content` (Markdown) to a styled `.docx` at `out_path`. Raises
    RuntimeError if pandoc is unavailable or the conversion fails."""
    pandoc = _resolve_bin()
    if pandoc is None:
        raise RuntimeError("pandoc is not available")

    reference = ensure_reference_docx()
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    md = _compose_markdown(content, title)

    proc = subprocess.run(
        [
            pandoc,
            "--from",
            "gfm",  # GitHub-flavoured Markdown: tables, task lists, strikethrough
            "--to",
            "docx",
            f"--reference-doc={reference}",
            "--output",
            str(Path(out_path).resolve()),
        ],
        input=md.encode("utf-8"),
        capture_output=True,
        timeout=settings.pandoc_timeout,
    )
    if proc.returncode != 0 or not Path(out_path).is_file():
        raise RuntimeError(
            f"pandoc conversion failed (rc={proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace')[:500]}"
        )
    return out_path
