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

"""Boilerplate removal for fetched pages: trafilatura (benchmark leader,
Apache-2.0) first, readability-lxml as the fallback, bare lxml text as the
last resort. Returns the main text plus the
publish date and title when the page declares them."""

import json
import logging
from dataclasses import dataclass

_log = logging.getLogger("pai.web.extract")

# A page whose extraction lands under this is "near-empty" — likely a
# JS-rendered shell; the pipeline may escalate to Playwright rendering.
NEAR_EMPTY_CHARS = 500


@dataclass
class Extracted:
    text: str
    title: str | None
    published_date: str | None  # ISO YYYY-MM-DD when declared


def _iso_date(value: str | None) -> str | None:
    if not value or not isinstance(value, str):
        return None
    v = value.strip()[:10]
    # Trafilatura normalises to YYYY-MM-DD; keep only that shape.
    if len(v) == 10 and v[4] == "-" and v[7] == "-":
        return v
    return None


def extract(html: str, url: str) -> Extracted:
    """Best-effort main-content extraction. Never raises; an unusable page
    comes back as empty text (the caller treats it as near-empty)."""
    if not html or not html.strip():
        return Extracted(text="", title=None, published_date=None)

    # Tier 1 — trafilatura (JSON output carries title + date metadata).
    try:
        import trafilatura

        raw = trafilatura.extract(
            html, url=url, output_format="json", with_metadata=True,
            include_comments=False,
        )
        if raw:
            doc = json.loads(raw)
            text = (doc.get("text") or "").strip()
            if text:
                return Extracted(
                    text=text,
                    title=(doc.get("title") or None),
                    published_date=_iso_date(doc.get("date")),
                )
    except Exception as e:  # noqa: BLE001 — fall through to the next tier
        _log.debug("trafilatura failed for %s: %s", url, e)

    # Tier 2 — readability-lxml.
    try:
        from lxml import html as lxml_html
        from readability import Document

        doc = Document(html)
        summary = doc.summary(html_partial=True)
        text = lxml_html.fromstring(summary).text_content().strip()
        if text:
            return Extracted(text=text, title=doc.short_title() or None, published_date=None)
    except Exception as e:  # noqa: BLE001
        _log.debug("readability failed for %s: %s", url, e)

    # Tier 3 — bare text (better than nothing for plain pages).
    try:
        from lxml import html as lxml_html

        tree = lxml_html.fromstring(html)
        for bad in tree.xpath("//script|//style|//noscript"):
            bad.getparent().remove(bad)
        text = " ".join(tree.text_content().split())
        title_el = tree.find(".//title")
        title = title_el.text.strip() if title_el is not None and title_el.text else None
        return Extracted(text=text, title=title, published_date=None)
    except Exception as e:  # noqa: BLE001
        _log.debug("bare extraction failed for %s: %s", url, e)
        return Extracted(text="", title=None, published_date=None)
