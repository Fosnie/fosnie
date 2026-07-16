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

"""The evidence memory bank (recipe steps 1–2):
every source gets a stable ID and a structured note; the writer only ever sees
notes and only ever cites IDs that resolve here — fabricated references become
structurally impossible. Two namespaces: web sources are `W1`, `W2`,
… and the user's own documents are `D1`, `D2`, … so provenance is segregated
in the report (separate reference sections). Raw chunks are retained per source
for the Phase-3 citation-verification pass."""

from dataclasses import dataclass, field

from .. import reranker
from ..web.pipeline import _Source


@dataclass
class DocSource:
    """A corpus document as an evidence source. Duck-compatible with the web
    `_Source` (every attribute the Phase-1 helpers touch is present) so notes/
    outline/writer treat web and doc sources uniformly. `text`/`chunks` carry
    the evidence; the web-flavoured fields take doc-sensible defaults."""

    doc_id: str
    kb_id: str
    kb_name: str
    filename: str
    mime: str | None = None
    path: str = ""
    chunks: list[str] = field(default_factory=list)
    # Anchor for a citation row — set only on the retrieval-sampling path (which
    # has real chunk/page anchors); the whole-document census leaves them None
    # (a whole-text note has no single page anchor — honest).
    page_number: int | None = None
    chunk_index: int | None = None
    clause_section_ref: str | None = None
    # --- web-`_Source` compatibility (so existing helpers need no branching) --
    snippet_only: bool = False
    published_date: str | None = None
    fetched_at: str = ""

    @property
    def title(self) -> str:
        return self.filename

    @property
    def domain(self) -> str:
        # Used in meta lines; for a document this is the owning library.
        return self.kb_name

    @property
    def url(self) -> str:
        # Dedup key in the bank; documents dedup on doc_id, not URL — kept unique
        # so a doc never collides with a web source in the web `_by_url` map.
        return f"doc://{self.doc_id}"


@dataclass
class Note:
    claims: list[str]
    quotes: list[str]
    # Set only on the stuff-whole-corpus fast path: the writer reads the full
    # document text directly (no per-doc note LLM call was made).
    full_text: str | None = None

    def text(self) -> str:
        if self.full_text:
            return self.full_text
        parts = []
        if self.claims:
            parts.append("Claims:\n" + "\n".join(f"- {c}" for c in self.claims))
        if self.quotes:
            parts.append("Quotes:\n" + "\n".join(f'- "{q}"' for q in self.quotes))
        return "\n".join(parts)


@dataclass
class SourceRecord:
    sid: str  # "W{n}" (web) or "D{n}" (document)
    source: object  # _Source | DocSource
    note: Note | None = None
    kind: str = "web"  # "web" | "doc"

    def meta_line(self) -> str:
        """The `[ID] title — origin` line shown to the outline/writer calls."""
        s = self.source
        if self.kind == "doc":
            return f"[{self.sid}] {s.filename} — {s.kb_name} (your documents)"
        meta = f"[{self.sid}] {s.title} — {s.domain}"
        if s.published_date:
            meta += f", published {s.published_date}"
        return meta


@dataclass
class Bank:
    records: list[SourceRecord] = field(default_factory=list)
    _by_url: dict[str, str] = field(default_factory=dict)
    _by_doc_id: dict[str, str] = field(default_factory=dict)
    _by_sid: dict[str, SourceRecord] = field(default_factory=dict)
    _n_web: int = 0
    _n_doc: int = 0

    def add_source(self, src: _Source) -> str:
        """Register a web source; re-adding the same URL returns its existing ID."""
        existing = self._by_url.get(src.url)
        if existing is not None:
            return existing
        self._n_web += 1
        sid = f"W{self._n_web}"
        rec = SourceRecord(sid=sid, source=src, kind="web")
        self.records.append(rec)
        self._by_url[src.url] = sid
        self._by_sid[sid] = rec
        return sid

    def add_doc_source(self, doc: DocSource) -> str:
        """Register a corpus document; re-adding the same doc_id returns its ID."""
        existing = self._by_doc_id.get(doc.doc_id)
        if existing is not None:
            return existing
        self._n_doc += 1
        sid = f"D{self._n_doc}"
        rec = SourceRecord(sid=sid, source=doc, kind="doc")
        self.records.append(rec)
        self._by_doc_id[doc.doc_id] = sid
        self._by_sid[sid] = rec
        return sid

    def get(self, sid: str) -> SourceRecord | None:
        return self._by_sid.get(sid)

    def resolve(self, sids: list[str]) -> list[SourceRecord]:
        """The records for `sids`, dropping unknown IDs (order preserved)."""
        out = []
        for sid in sids:
            rec = self._by_sid.get(sid)
            if rec is not None:
                out.append(rec)
        return out

    def sids(self) -> list[str]:
        return [r.sid for r in self.records]

    def web_records(self) -> list[SourceRecord]:
        return [r for r in self.records if r.kind == "web"]

    def doc_records(self) -> list[SourceRecord]:
        return [r for r in self.records if r.kind == "doc"]


async def from_pool_sources(question: str, sources: list[_Source], cap: int) -> Bank:
    """Build a bank from collected sources, capped to the budget's
    `max_sources` by reranking lead chunks against the research question.
    Fetched sources outrank snippet-only ones at equal relevance (richer
    evidence wins ties); degraded reranker (all-equal) preserves that order."""
    usable = [s for s in sources if s.chunks]
    if len(usable) > cap:
        leads = [s.chunks[0] for s in usable]
        scores = await reranker.rerank(question, leads)
        ranked = sorted(
            zip(usable, scores),
            key=lambda x: (x[1], not x[0].snippet_only),
            reverse=True,
        )
        usable = [s for s, _ in ranked[:cap]]
    bank = Bank()
    for s in usable:
        bank.add_source(s)
    return bank
