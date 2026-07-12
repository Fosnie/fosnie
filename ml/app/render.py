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

"""DOCX→PDF rendition via LibreOffice headless. Swappable: the engine is `soffice` here, fronted by `available()`
so the platform degrades cleanly (HTTP 503) where LibreOffice is absent rather
than failing opaquely."""

from __future__ import annotations

import glob
import shutil
import subprocess
from pathlib import Path

from .config import settings

# Common LibreOffice install locations checked when `soffice` is not on PATH,
# across the platform's deployment targets. `SOFFICE_BIN` (config) always wins.
_FALLBACKS = [
    # macOS (Mac Studio profile)
    "/Applications/LibreOffice.app/Contents/MacOS/soffice",
    # Linux (vLLM/GPU profile)
    "/usr/bin/soffice",
    "/usr/local/bin/soffice",
    "/opt/libreoffice/program/soffice",
    # Windows (dev)
    r"C:\Program Files\LibreOffice\program\soffice.exe",
    r"C:\Program Files (x86)\LibreOffice\program\soffice.exe",
]
# Linux glob for versioned /opt installs (e.g. /opt/libreoffice7.6/program/soffice).
_FALLBACK_GLOBS = ["/opt/libreoffice*/program/soffice"]


def _resolve_bin() -> str | None:
    found = shutil.which(settings.soffice_bin)
    if found:
        return found
    for cand in _FALLBACKS:
        if Path(cand).is_file():
            return cand
    for pattern in _FALLBACK_GLOBS:
        for cand in glob.glob(pattern):
            if Path(cand).is_file():
                return cand
    return None


def available() -> bool:
    return _resolve_bin() is not None


def docx_to_pdf(in_path: str, out_dir: str) -> str:
    """Convert `in_path` to PDF in `out_dir`; return the PDF path. Raises
    RuntimeError when LibreOffice is unavailable or conversion fails."""
    soffice = _resolve_bin()
    if soffice is None:
        raise RuntimeError("LibreOffice (soffice) is not available")

    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)
    # soffice writes <stem>.pdf into the outdir. A dedicated user profile keeps
    # concurrent conversions from clashing on the default profile lock.
    proc = subprocess.run(
        [
            soffice,
            "--headless",
            "--nologo",
            "--nolockcheck",
            f"-env:UserInstallation=file:///{out.resolve().as_posix()}/.lo_profile",
            "--convert-to",
            "pdf",
            "--outdir",
            str(out.resolve()),
            str(Path(in_path).resolve()),
        ],
        capture_output=True,
        timeout=settings.soffice_timeout,
    )
    pdf_path = out / (Path(in_path).stem + ".pdf")
    if proc.returncode != 0 or not pdf_path.is_file():
        raise RuntimeError(
            f"soffice conversion failed (rc={proc.returncode}): "
            f"{proc.stderr.decode('utf-8', 'replace')[:500]}"
        )
    return str(pdf_path)
