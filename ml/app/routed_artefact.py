# Copyright 2026 Private AI Ltd (SC881079)
# SPDX-License-Identifier: Apache-2.0
"""Deterministic, no-inference draft renderer. No input/output path or overrides.

Temporary output belongs to this synchronous worker and is removed even if the
HTTP caller disconnects. Core alone publishes the returned bytes. Images, raw HTML
and external resources are never loaded by these text-only builders.
"""
import base64
import hashlib
import html
from pathlib import Path
from tempfile import TemporaryDirectory
from typing import Literal

from pydantic import BaseModel, ConfigDict, Field, model_validator

MAX_BYTES = 4 * 1024 * 1024
MIMES = {
    "md": "text/markdown",
    "docx": "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "pdf": "application/pdf",
}


class RenderRequest(BaseModel):
    model_config = ConfigDict(extra="forbid", frozen=True)
    schema_version: Literal[1]
    kind: Literal["md", "docx", "pdf"]
    title: str = Field(max_length=160)
    content: str = Field(min_length=1, max_length=1024 * 1024)

    @model_validator(mode="after")
    def bounded(self):
        if len(self.content.encode("utf-8")) > 1024 * 1024 or not self.content.strip():
            raise ValueError("draft exceeds the byte limit or is empty")
        return self


def deny_resource(url, *args, **kwargs):
    raise ValueError("routed renderer cannot load resources")


def render(req: RenderRequest) -> dict:
    # No deployment template, pandoc, LibreOffice, LLM client or URL fetcher.
    with TemporaryDirectory(prefix="fosnie-routed-render-") as directory:
        path = Path(directory) / f"document.{req.kind}"
        if req.kind == "md":
            text = req.content if req.content.lstrip().startswith("# ") else f"# {req.title}\n\n{req.content}"
            path.write_text(text + "\n", encoding="utf-8", newline="\n")
        elif req.kind == "docx":
            from .generate import _build_docx
            from .validators import validate_docx

            # Structural python-docx builder: Markdown image/HTML text is text.
            _build_docx(req.title, req.content, str(path))
            validate_docx(str(path))
        else:
            from markdown_it import MarkdownIt
            from weasyprint import HTML
            from .validators import validate_pdf

            md = MarkdownIt("commonmark", {"html": False}).enable("table").disable("image")
            body = md.render(req.content)
            title = "" if req.content.lstrip().startswith("# ") else f"<h1>{html.escape(req.title)}</h1>"
            document = (
                '<!doctype html><html><head><meta charset="utf-8"><style>'
                '@page { size: A4; margin: 20mm } body { font: 11pt sans-serif; overflow-wrap: anywhere } '
                'table { border-collapse: collapse } td, th { border: 1px solid #aaa; padding: 5px }'
                '</style></head><body>' + title + body + '</body></html>'
            )
            HTML(string=document, url_fetcher=deny_resource).write_pdf(str(path))
            validate_pdf(str(path))
        if not 0 < path.stat().st_size <= MAX_BYTES:
            raise ValueError("rendered file exceeds the byte limit")
        data = path.read_bytes()
        return {"schema_version": 1, "kind": req.kind, "mime": MIMES[req.kind],
                "bytes_base64": base64.b64encode(data).decode("ascii"),
                "sha256": hashlib.sha256(data).hexdigest()}
