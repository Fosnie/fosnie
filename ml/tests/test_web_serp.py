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

"""SearXNG JSON parsing + recency mapping, and the DDG-HTML fallback parser."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.web.fallback_search import parse_ddg_html
from app.web.searxng import _TIME_RANGE, parse_results

_SEARXNG_BODY = {
    "query": "rust language",
    "results": [
        {
            "url": "https://rust-lang.org/",
            "title": "Rust Programming Language",
            "content": "Rust is blazingly fast and memory-efficient.",
            "engine": "google",
            "publishedDate": None,
        },
        {
            "url": "https://blog.rust-lang.org/releases/",
            "title": "Releases",
            "content": "Release announcements.",
            "engine": "duckduckgo",
            "publishedDate": "2026-05-28T00:00:00",
        },
        # Degenerate rows the parser must survive:
        {"url": "", "title": "no url", "content": "dropped"},
        {"title": "missing url entirely"},
        {"url": "https://example.com/", "title": None, "content": None, "engine": None},
    ],
}


def test_parse_results_shape_and_dates():
    out = parse_results(_SEARXNG_BODY, limit=10)
    assert len(out) == 3, "rows without a url are dropped"
    assert out[0].url == "https://rust-lang.org/"
    assert out[0].published_date is None
    assert out[1].published_date == "2026-05-28", "ISO date part only"
    assert out[2].title == "" and out[2].snippet == ""
    assert out[2].engine == "searxng", "missing engine falls back"


def test_parse_results_limit():
    assert len(parse_results(_SEARXNG_BODY, limit=1)) == 1
    assert parse_results(_SEARXNG_BODY, limit=0) == []
    assert parse_results({}, limit=5) == []


def test_recency_mapping():
    assert _TIME_RANGE.get("day") == "day"
    assert _TIME_RANGE.get("week") == "week"
    assert _TIME_RANGE.get("month") == "month"
    assert _TIME_RANGE.get("year") == "year"
    assert _TIME_RANGE.get("any") is None, "'any' omits time_range"
    assert _TIME_RANGE.get("bogus") is None, "unknown values omit time_range"


_DDG_HTML = """
<html><body>
  <div class="results">
    <div class="result results_links">
      <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F&amp;rut=abc">Rust Programming Language</a>
      <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Frust-lang.org%2F">Rust is fast.</a>
    </div>
    <div class="result results_links">
      <a class="result__a" href="https://releases.rs/">Rust Versions</a>
      <div class="result__snippet">All releases.</div>
    </div>
    <div class="result results_links">
      <a class="result__a" href="javascript:void(0)">junk link</a>
    </div>
  </div>
</body></html>
"""


def test_parse_ddg_html_unwraps_redirects():
    out = parse_ddg_html(_DDG_HTML, limit=10)
    assert len(out) == 2, "javascript: links are dropped"
    assert out[0].url == "https://rust-lang.org/"
    assert out[0].title == "Rust Programming Language"
    assert out[0].snippet == "Rust is fast."
    assert out[1].url == "https://releases.rs/"
    assert out[1].snippet == "All releases."
    assert all(r.engine == "ddg-html" for r in out)


def test_parse_ddg_html_limit():
    assert len(parse_ddg_html(_DDG_HTML, limit=1)) == 1
