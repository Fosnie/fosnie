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

"""Syndication dedup: shingle-Jaccard near-duplicate detection."""

import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1]))

from app.web.dedup import is_near_duplicate, jaccard, shingles

_WIRE = (
    "LONDON (Reuters) - The Bank of England held interest rates steady on "
    "Thursday, as policymakers weighed persistent services inflation against "
    "signs of a cooling labour market. Governor Andrew Bailey said the committee "
    "needed more evidence that price pressures were easing before cutting rates. "
    "Markets had priced in a hold, with the first cut now expected in autumn."
)

# The same wire copy republished with an outlet preamble and minor edits.
_SYNDICATED = (
    "Business news: " + _WIRE.replace("on Thursday", "this Thursday").replace(
        "Markets had priced in a hold", "Markets had largely priced in a hold"
    )
)

_DISTINCT = (
    "The European Central Bank cut its deposit rate by 25 basis points, the "
    "third reduction this year, citing faster-than-expected disinflation across "
    "the euro area. President Christine Lagarde signalled further easing would "
    "depend on wage data due next quarter, disappointing investors who had "
    "hoped for a clearer commitment to consecutive cuts."
)


def test_identical_is_duplicate():
    pool = [shingles(_WIRE)]
    assert is_near_duplicate(_WIRE, pool, threshold=0.6)


def test_syndicated_copy_is_duplicate_at_06():
    pool = [shingles(_WIRE)]
    assert is_near_duplicate(_SYNDICATED, pool, threshold=0.6)


def test_distinct_articles_not_duplicate():
    pool = [shingles(_WIRE)]
    assert not is_near_duplicate(_DISTINCT, pool, threshold=0.6)


def test_empty_and_short_text_safe():
    pool = [shingles(_WIRE)]
    assert not is_near_duplicate("", pool, threshold=0.6)
    assert not is_near_duplicate("   ", pool, threshold=0.6)
    assert shingles("two words", k=5) == {"two words"}
    assert shingles("") == set()


def test_jaccard_bounds():
    a, b = shingles(_WIRE), shingles(_DISTINCT)
    assert 0.0 <= jaccard(a, b) < 0.2
    assert jaccard(a, a) == 1.0
    assert jaccard(set(), a) == 0.0
