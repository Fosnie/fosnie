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

"""Extraction tiers: trafilatura → readability → bare text. Never raises."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.web import extractor

_ARTICLE = """
<html>
<head>
  <title>Rust 1.92 released</title>
  <meta property="article:published_time" content="2026-05-28T10:00:00Z">
</head>
<body>
  <nav><a href="/">Home</a><a href="/about">About</a></nav>
  <article>
    <h1>Rust 1.92 released</h1>
    <p>The Rust team has announced version 1.92 of the language. This release
    brings several improvements to the borrow checker and stabilises a number
    of long-awaited APIs that the community has been requesting for years.</p>
    <p>Developers can upgrade by running rustup update stable as usual. The
    full release notes cover every change in detail, including the new lints
    and the platform support adjustments that ship alongside this release.</p>
  </article>
  <footer>Copyright 2026. Privacy. Terms. Cookie settings.</footer>
</body>
</html>
"""


def test_article_extraction_with_metadata():
    ex = extractor.extract(_ARTICLE, "https://example.com/rust-192")
    assert "borrow checker" in ex.text
    assert "Cookie settings" not in ex.text, "boilerplate stripped"
    assert ex.title and "Rust 1.92" in ex.title
    assert ex.published_date == "2026-05-28"


def test_empty_html():
    ex = extractor.extract("", "https://example.com/")
    assert ex.text == "" and ex.title is None and ex.published_date is None
    ex2 = extractor.extract("   \n  ", "https://example.com/")
    assert ex2.text == ""


def test_garbage_html_never_raises():
    ex = extractor.extract("<<<<not really html>>>> &&& <div", "https://example.com/")
    assert isinstance(ex.text, str)  # whatever came back, it's text, no exception


def test_minimal_page_falls_through_tiers():
    # Too thin for trafilatura's article detection — the lower tiers still
    # return the visible text rather than nothing.
    html = "<html><head><title>Tiny</title></head><body><p>Just one short line.</p><script>evil()</script></body></html>"
    ex = extractor.extract(html, "https://example.com/tiny")
    assert "Just one short line." in ex.text
    assert "evil()" not in ex.text, "scripts stripped in the bare-text tier"


def test_near_empty_threshold_constant():
    assert extractor.NEAR_EMPTY_CHARS == 500
